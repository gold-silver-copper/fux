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

#[path = "viewer/painted.rs"]
mod painted;

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

use bevy_asset::Assets;
use fux::assets::{ConfigAsset, ConfigHandle, ThemeToken};
use fux::config::ClipboardPolicy;
use fux::model::{Ids, InstanceNode, NodeId, PaneId, PointerKind, Shows, Surface, ViewerRequest};
use fux::surface::Text as SurfaceText;
use fux::viewer::input::{Input, Translator};
use fux::viewer::keys::{self, KeyChord};
use fux::viewer::replicate::EntityMap;
use fux::viewer::theme::CellStyle;
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
            r.register::<Surface>();
            r.register::<SurfaceText>();
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

    /// `[pane 1 | surface]`: pane 1 fills the left half; the right half is a surface leaf
    /// (node 3) holding one streamed text row (node 4). `leaves[1]` is the surface leaf.
    fn beside_surface() -> Self {
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
        let surface = world
            .spawn((
                InstanceNode,
                NodeId(3),
                ChildOf(root),
                Surface,
                Node {
                    flex_direction: FlexDirection::Column,
                    flex_grow: 1.0,
                    flex_basis: Val::Px(0.0),
                    ..Default::default()
                },
            ))
            .id();
        world.spawn((
            InstanceNode,
            NodeId(4),
            ChildOf(surface),
            SurfaceText("tasks".into()),
            Node {
                height: Val::Px(1.0),
                ..Default::default()
            },
        ));
        Self {
            world,
            registry: Self::registry(),
            root,
            leaves: vec![a, surface],
            panes: vec![pane_a],
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
            .allow_component::<Surface>()
            .allow_component::<SurfaceText>()
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
    app.insert_resource(ClipboardPolicy::WriteOnly);
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

// ---------------------------------------------------------------------------------------------
// (8) configuration assets: theme, prefix and bindings, clipboard policy
// ---------------------------------------------------------------------------------------------

/// A config directory the viewer loads `fux.toml` from (canonical: the watcher needs it).
struct ConfigDir {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
}

impl ConfigDir {
    fn new(document: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let this = Self { _dir: dir, root };
        this.write(document);
        this
    }

    fn write(&self, document: &str) {
        std::fs::write(self.root.join("fux.toml"), document).unwrap();
    }

    fn viewer(&self) -> App {
        viewer::build_in(80, 24, &self.root, std::sync::Arc::new(|| {}))
    }
}

/// Steps the viewer until `done` holds or two seconds pass (the watcher debounces 300 ms).
fn settle(app: &mut App, mut done: impl FnMut(&mut App) -> bool) -> bool {
    let start = std::time::Instant::now();
    loop {
        app.update();
        if done(app) {
            return true;
        }
        if start.elapsed() > std::time::Duration::from_secs(2) {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn cell_bg(app: &App, col: u16, row: u16) -> Option<fux::wire::Color> {
    app.world()
        .resource::<Painter>()
        .screen()
        .get(col, row)
        .map(|c| c.style.bg)
}

#[test]
fn theme_tokens_resolve_to_the_configured_colours_and_follow_reloads() {
    use fux::wire::Color;
    let dir = ConfigDir::new("[style]\nbar-background = 'blue'\ntab-active = 'default'\n");
    let scene = ServerScene::grid(1, 2);
    let mut app = dir.viewer();
    let mut frame = scene.frame(1);
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
    let default_bar = ThemeToken::BAR_BACKGROUND.default_color();
    assert!(
        matches!(cell_bg(&app, 79, 23), Some(bg) if bg == default_bar || bg == Color::Indexed(4)),
        "before the asset lands the bar wears today's default"
    );
    assert!(
        settle(&mut app, |app| cell_bg(app, 79, 23)
            == Some(Color::Indexed(4))),
        "the bar-background token paints the configured colour: {:?}",
        cell_bg(&app, 79, 23)
    );
    let active = app
        .world()
        .resource::<Painter>()
        .screen()
        .get(0, 23)
        .unwrap()
        .style;
    assert_eq!(
        active.attrs & Style::INVERSE,
        Style::INVERSE,
        "`tab-active = default` draws the active tab reversed"
    );
    assert_eq!(active.bg, Color::Indexed(4), "over the bar's fill");

    dir.write("[style]\nbar-background = 'red'\n");
    assert!(
        settle(&mut app, |app| cell_bg(app, 79, 23)
            == Some(Color::Indexed(1))),
        "a theme reload repaints the bar: {:?}",
        cell_bg(&app, 79, 23)
    );
    let active = app
        .world()
        .resource::<Painter>()
        .screen()
        .get(0, 23)
        .unwrap()
        .style;
    assert_eq!(
        active.bg,
        ThemeToken::TAB_ACTIVE.default_color(),
        "a token dropped from the file returns to its default"
    );
    assert_eq!(
        active.fg,
        Color::Indexed(1),
        "reversed accents take the bar's fill as text"
    );

    // A newly spawned tokened entity picks up the current theme without a reload.
    let bar = app
        .world_mut()
        .spawn((ThemeToken::BAR_BACKGROUND, bevy_ui::Node::default()))
        .id();
    app.update();
    assert_eq!(
        app.world().get::<CellStyle>(bar).map(|s| s.0.bg),
        Some(Color::Indexed(1))
    );

    // An invalid edit keeps the previous theme.
    dir.write("[style]\nbar-background = 'purple'\n");
    std::thread::sleep(std::time::Duration::from_millis(600));
    settle(&mut app, |_| false);
    assert_eq!(cell_bg(&app, 79, 23), Some(Color::Indexed(1)));
}

#[test]
fn configured_prefix_and_bindings_apply_and_reload() {
    let dir = ConfigDir::new("prefix = 'C-a'\n[bindings]\n'|' = 'split-side'\n'%' = 'help'\n");
    let scene = ServerScene::grid(2, 2);
    let mut app = dir.viewer();
    push_frame(&mut app, scene.frame(1));
    assert!(
        settle(&mut app, |app| *app
            .world()
            .resource::<fux::viewer::focus::Bindings>()
            .prefix()
            == KeyChord::ctrl('a')),
        "the configured prefix is loaded"
    );
    take_requests(&mut app);

    // The old prefix is now plain input; the new one enters prefix mode.
    push_keys(&mut app, [key(TKey::Char('b'), Modifiers::CONTROL)]);
    app.update();
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::Input(b"\x02".to_vec())]
    );
    push_keys(
        &mut app,
        [
            key(TKey::Char('a'), Modifiers::CONTROL),
            key(TKey::Char('|'), Modifiers::NONE),
        ],
    );
    app.update();
    assert!(matches!(
        take_requests(&mut app).as_slice(),
        [ViewerRequest::Split {
            direction: fux::model::SplitDirection::Right,
            template: None
        }]
    ));
    // `%` was remapped away from split-side; `h` keeps its default.
    push_keys(
        &mut app,
        [
            key(TKey::Char('a'), Modifiers::CONTROL),
            key(TKey::Char('%'), Modifiers::NONE),
        ],
    );
    app.update();
    assert!(take_requests(&mut app).is_empty(), "help is local");
    let leaf = |app: &App, i: usize| local(app, scene.leaves[i]);
    push_keys(
        &mut app,
        [
            key(TKey::Char('a'), Modifiers::CONTROL),
            key(TKey::Char('l'), Modifiers::NONE),
        ],
    );
    app.update();
    assert_eq!(
        focused(&app),
        Some(leaf(&app, 1)),
        "defaults stay underneath"
    );
    take_requests(&mut app);
    // Prefix twice sends the prefix itself.
    push_keys(
        &mut app,
        [
            key(TKey::Char('a'), Modifiers::CONTROL),
            key(TKey::Char('a'), Modifiers::CONTROL),
        ],
    );
    app.update();
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::Input(b"\x01".to_vec())]
    );

    // A reload rebinds without reconnecting: the prefix returns to C-b and `|` is unbound.
    dir.write("[bindings]\nEsc = 'detach'\n");
    assert!(
        settle(&mut app, |app| *app
            .world()
            .resource::<fux::viewer::focus::Bindings>()
            .prefix()
            == KeyChord::ctrl('b')),
        "the reload lands"
    );
    take_requests(&mut app);
    push_keys(
        &mut app,
        [
            key(TKey::Char('b'), Modifiers::CONTROL),
            key(TKey::Escape, Modifiers::NONE),
        ],
    );
    app.update();
    assert_eq!(take_requests(&mut app), vec![ViewerRequest::Detach]);
    push_keys(
        &mut app,
        [
            key(TKey::Char('b'), Modifiers::CONTROL),
            key(TKey::Char('|'), Modifiers::NONE),
        ],
    );
    app.update();
    assert!(take_requests(&mut app).is_empty(), "`|` is unbound again");
}

#[test]
fn clipboard_policy_gates_osc_52_both_ways() {
    let dir = ConfigDir::new("clipboard = 'disabled'\n");
    let scene = ServerScene::grid(1, 2);
    let mut app = dir.viewer();
    push_frame(&mut app, scene.frame(1));
    assert!(
        settle(&mut app, |app| app
            .world()
            .resource::<Assets<ConfigAsset>>()
            .contains(&app.world().resource::<ConfigHandle>().0)),
        "fux.toml loads"
    );
    let out = |app: &mut App| core::mem::take(&mut app.world_mut().resource_mut::<Painter>().out);
    out(&mut app);
    let osc = b"\x1b]52;c;aGVsbG8=\x07";
    let clipboard_frame = |seq: u64| {
        let mut update = delta(2, 40, 23, "world");
        update.full = false;
        update.seq = seq;
        update.clipboard = Some("aGVsbG8=".into());
        SceneFrame {
            revision: seq,
            full: false,
            scene: String::new(),
            roots: None,
            terminals: vec![update],
            ..scene.frame(seq)
        }
    };
    push_frame(&mut app, clipboard_frame(2));
    app.update();
    assert!(
        !out(&mut app).windows(osc.len()).any(|w| w == osc),
        "`disabled` drops the write"
    );

    dir.write("clipboard = 'write-only'\n");
    assert!(
        settle(&mut app, |app| *app.world().resource::<ClipboardPolicy>()
            == ClipboardPolicy::WriteOnly),
        "the policy follows the edit"
    );
    out(&mut app);
    push_frame(&mut app, clipboard_frame(3));
    app.update();
    let bytes = out(&mut app);
    assert_eq!(
        bytes.windows(osc.len()).filter(|w| *w == osc).count(),
        1,
        "`write-only` writes once: {:?}",
        String::from_utf8_lossy(&bytes)
    );

    dir.write("clipboard = 'off'\n");
    assert!(
        settle(&mut app, |app| *app.world().resource::<ClipboardPolicy>()
            == ClipboardPolicy::Off),
        "and back"
    );
    out(&mut app);
    push_frame(&mut app, clipboard_frame(4));
    app.update();
    assert!(!out(&mut app).windows(osc.len()).any(|w| w == osc));
}

// ---------------------------------------------------------------------------------------------
// (9) choosers, prompts, confirmations, copy mode, focus ring
// ---------------------------------------------------------------------------------------------

mod common;

use bevy_state::prelude::State;
use bevy_ui::interaction_states::Selected;
use fux::viewer::choosers::{ActiveDescendant, Chooser, ChooserKind};
use fux::viewer::chrome::Popup;
use fux::viewer::copy_mode::CopyView;
use fux::viewer::focus::FocusRing;
use fux::viewer::prompts::{Confirmation, Prompt};
use fux::viewer::{Brp, BrpReply, BrpTag, Mode, Reconnect};
use fux::wire::Welcome;

fn prefix() -> Event {
    key(TKey::Char('b'), Modifiers::CONTROL)
}

fn chord(app: &mut App, c: char) {
    push_keys(app, [prefix(), key(TKey::Char(c), Modifiers::NONE)]);
    app.update();
}

fn press(app: &mut App, code: TKey, modifiers: Modifiers) {
    push_keys(app, [key(code, modifiers)]);
    app.update();
}

fn type_str(app: &mut App, text: &str) {
    push_keys(
        app,
        text.chars().map(|c| key(TKey::Char(c), Modifiers::NONE)),
    );
    app.update();
}

fn mode(app: &App) -> Mode {
    *app.world().resource::<State<Mode>>().get()
}

fn popups(app: &mut App) -> Vec<Entity> {
    app.world_mut()
        .query_filtered::<Entity, With<Popup>>()
        .iter(app.world())
        .collect()
}

/// The painted text of one screen row, surrounding blanks trimmed.
fn row_text(app: &App, row: u16) -> String {
    let screen = app.world().resource::<Painter>().screen();
    let text: String = (0..screen.cols())
        .filter_map(|col| screen.get(col, row))
        .filter(|c| c.width > 0)
        .map(|c| c.text.as_str().to_owned())
        .collect();
    text.trim().to_owned()
}

fn cell_attrs(app: &App, col: u16, row: u16) -> u8 {
    app.world()
        .resource::<Painter>()
        .screen()
        .get(col, row)
        .map_or(0, |c| c.style.attrs)
}

/// The chooser's rows in order with their selection state.
fn chooser_rows(app: &mut App) -> Vec<(String, bool)> {
    let world = app.world_mut();
    let Ok(popup) = world
        .query_filtered::<Entity, With<Chooser>>()
        .single(world)
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut rows = world.query::<(&fux::viewer::Text, Has<Selected>)>();
    for row in world.entity(popup).get::<Children>().unwrap().iter() {
        let Some(list) = world.entity(row).get::<Children>() else {
            continue;
        };
        if world.entity(row).get::<ActiveDescendant>().is_none() {
            continue;
        }
        for item in list.iter() {
            if let Ok((text, selected)) = rows.get(world, item) {
                out.push((text.0.clone(), selected));
            }
        }
    }
    out
}

fn welcome(workspace: &str) -> ServerFrame {
    ServerFrame::Welcome(Welcome {
        viewer: fux::model::ViewerId(1),
        instance: "i".into(),
        workspace: workspace.into(),
    })
}

fn two_roots(scene: &ServerScene) -> SceneFrame {
    SceneFrame {
        roots: Some(vec![
            RootEntry {
                node: NodeId(1),
                name: "main".into(),
            },
            RootEntry {
                node: NodeId(9),
                name: "other".into(),
            },
        ]),
        ..scene.frame(1)
    }
}

#[test]
fn tab_chooser_lists_roots_and_enter_shows_the_chosen_one() {
    let scene = ServerScene::grid(1, 1);
    let mut app = viewer::build(80, 24);
    push_frame(&mut app, two_roots(&scene));
    app.update();
    let leaf = local(&app, scene.leaves[0]);
    take_requests(&mut app);

    chord(&mut app, 'w');
    assert_eq!(mode(&app), Mode::Chooser);
    assert_eq!(popups(&mut app).len(), 1);
    assert_eq!(
        chooser_rows(&mut app),
        vec![
            (" 1:main ".to_owned(), true),
            (" 2:other ".to_owned(), false)
        ],
        "the shown root starts under the cursor"
    );
    assert_ne!(focused(&app), Some(leaf), "the popup holds focus");
    // The list sits above the tab strip (bottom-left), the cursor row reversed.
    assert_eq!(row_text(&app, 20), "tabs");
    assert_eq!(row_text(&app, 21), "1:main");
    assert_eq!(row_text(&app, 22), "2:other");
    let active = ThemeToken::TAB_ACTIVE.default_color();
    assert_eq!(
        cell_bg(&app, 1, 21),
        Some(active),
        "the cursor row is reversed"
    );
    assert_ne!(cell_bg(&app, 1, 22), Some(active));

    press(&mut app, TKey::Char('j'), Modifiers::NONE);
    assert_eq!(
        chooser_rows(&mut app).iter().position(|(_, s)| *s),
        Some(1),
        "j moves the cursor"
    );
    press(&mut app, TKey::Char('j'), Modifiers::NONE);
    assert_eq!(
        chooser_rows(&mut app).iter().position(|(_, s)| *s),
        Some(0),
        "and wraps"
    );
    press(&mut app, TKey::Up, Modifiers::NONE);
    assert_eq!(chooser_rows(&mut app).iter().position(|(_, s)| *s), Some(1));
    press(&mut app, TKey::Enter, Modifiers::NONE);
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::Show(NodeId(9))]
    );
    assert_eq!(mode(&app), Mode::Normal);
    assert!(popups(&mut app).is_empty(), "Enter closes the chooser");
    assert_eq!(focused(&app), Some(leaf), "focus returns to the pane");
    assert_eq!(row_text(&app, 21), "", "and the popup is unpainted");

    // Escape cancels without a request; keys after it in the same batch reach the pane.
    chord(&mut app, 'w');
    push_keys(
        &mut app,
        [
            key(TKey::Escape, Modifiers::NONE),
            key(TKey::Char('z'), Modifiers::NONE),
        ],
    );
    app.update();
    assert!(popups(&mut app).is_empty());
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::Input(b"z".to_vec())]
    );

    // The prefix chord closes the chooser and starts a new command.
    chord(&mut app, 'w');
    chord(&mut app, 'c');
    assert!(popups(&mut app).is_empty());
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::NewRoot { template: None }]
    );
}

#[test]
fn chooser_dismisses_when_a_click_moves_focus_away() {
    let scene = ServerScene::grid(1, 2);
    let mut app = viewer::build(80, 24);
    push_frame(&mut app, two_roots(&scene));
    app.update();
    take_requests(&mut app);
    chord(&mut app, 'w');
    assert_eq!(popups(&mut app).len(), 1);
    assert_eq!(mode(&app), Mode::Chooser);

    let requests = click(&mut app, 60, 5);
    assert!(
        popups(&mut app).is_empty(),
        "focus left the popup: it is gone"
    );
    assert_eq!(requests, vec![ViewerRequest::Target(PaneId(2))]);
    app.update();
    assert_eq!(mode(&app), Mode::Normal);
    assert_eq!(focused(&app), Some(local(&app, scene.leaves[1])));
    assert_eq!(row_text(&app, 21), "");
}

#[test]
fn workspace_chooser_fills_from_brp_and_reconnects_elsewhere() {
    let scene = ServerScene::grid(1, 1);
    let mut app = viewer::build(80, 24);
    app.world_mut()
        .resource_mut::<Inbox>()
        .frames
        .push(welcome("alpha"));
    push_frame(&mut app, scene.frame(1));
    app.update();
    take_requests(&mut app);

    chord(&mut app, 's');
    assert_eq!(mode(&app), Mode::Chooser);
    assert_eq!(
        chooser_rows(&mut app),
        vec![(" loading… ".to_owned(), true)],
        "the list waits for the worker's reply"
    );
    app.world().resource::<Brp>().deliver(BrpReply {
        tag: BrpTag::WorkspaceList,
        result: Ok(serde_json::json!({
            "workspaces": [
                { "name": "alpha", "open": true, "viewers": 1, "roots": [] },
                { "name": "beta", "open": true, "viewers": 0, "roots": [] },
            ]
        })),
    });
    app.update();
    assert_eq!(
        chooser_rows(&mut app),
        vec![
            (" * alpha ".to_owned(), true),
            ("   beta ".to_owned(), false)
        ],
        "the current workspace is marked and under the cursor"
    );
    press(&mut app, TKey::Enter, Modifiers::NONE);
    assert!(
        app.world().get_resource::<Reconnect>().is_none(),
        "choosing the current workspace is a no-op"
    );
    assert!(popups(&mut app).is_empty());

    chord(&mut app, 's');
    app.world().resource::<Brp>().deliver(BrpReply {
        tag: BrpTag::WorkspaceList,
        result: Ok(serde_json::json!({
            "workspaces": [
                { "name": "alpha", "open": true, "viewers": 1, "roots": [] },
                { "name": "beta", "open": true, "viewers": 0, "roots": [] },
            ]
        })),
    });
    app.update();
    press(&mut app, TKey::Char('j'), Modifiers::NONE);
    press(&mut app, TKey::Enter, Modifiers::NONE);
    assert_eq!(
        app.world().get_resource::<Reconnect>(),
        Some(&Reconnect("beta".into())),
        "another workspace reconnects locally, without a ViewerRequest"
    );
    assert!(take_requests(&mut app).is_empty());
    assert_eq!(mode(&app), Mode::Normal);
}

#[test]
fn help_panel_lists_every_binding_and_scrolls_to_the_cursor() {
    let scene = ServerScene::grid(1, 1);
    let mut app = viewer::build(80, 12);
    push_frame(&mut app, scene.frame(1));
    app.update();
    chord(&mut app, '?');
    assert_eq!(mode(&app), Mode::Chooser);
    let rows = chooser_rows(&mut app);
    let table = app
        .world()
        .resource::<fux::viewer::focus::Bindings>()
        .table
        .clone();
    assert_eq!(rows.len(), table.bindings.len());
    assert!(rows.iter().any(|(t, _)| t.contains("copy-mode")));
    assert!(rows.iter().all(|(t, _)| t.starts_with(" C-b ")));
    let kind = {
        let world = app.world_mut();
        world
            .query::<&Chooser>()
            .single(world)
            .map(|c| c.kind)
            .unwrap()
    };
    assert_eq!(kind, ChooserKind::Help);
    assert_eq!(row_text(&app, 1), "bindings", "header above the rows");
    let first_visible = row_text(&app, 2);
    press(&mut app, TKey::Char('G'), Modifiers::NONE);
    let scroll = {
        let world = app.world_mut();
        world
            .query::<(&ActiveDescendant, &bevy_ui::ScrollPosition)>()
            .single(world)
            .map(|(_, s)| s.0.y)
            .unwrap()
    };
    assert!(scroll > 0.0, "the last row is scrolled into view");
    assert_ne!(row_text(&app, 2), first_visible, "the viewport moved");
    let last = rows.last().unwrap().0.trim().to_owned();
    assert_eq!(row_text(&app, 10), last, "last row just above the bar");
    press(&mut app, TKey::Char('q'), Modifiers::NONE);
    assert!(popups(&mut app).is_empty());
    assert_eq!(mode(&app), Mode::Normal);
}

#[test]
fn confirmations_commit_on_y_and_cancel_otherwise() {
    let scene = ServerScene::grid(1, 1);
    let mut app = viewer::build(80, 24);
    app.world_mut()
        .resource_mut::<Inbox>()
        .frames
        .push(welcome("alpha"));
    push_frame(&mut app, scene.frame(1));
    app.update();
    take_requests(&mut app);

    chord(&mut app, 'x');
    assert_eq!(mode(&app), Mode::Confirm);
    assert_eq!(
        row_text(&app, 22),
        "close pane 1? y/n",
        "a popover above the bar"
    );
    assert!(row_text(&app, 23).contains("CONFIRM"));
    press(&mut app, TKey::Char('n'), Modifiers::NONE);
    assert!(popups(&mut app).is_empty());
    assert!(take_requests(&mut app).is_empty(), "n sends nothing");
    assert_eq!(mode(&app), Mode::Normal);

    chord(&mut app, 'x');
    press(&mut app, TKey::Char('y'), Modifiers::NONE);
    assert_eq!(take_requests(&mut app), vec![ViewerRequest::ClosePane]);
    assert!(popups(&mut app).is_empty());

    chord(&mut app, 'K');
    let action = {
        let world = app.world_mut();
        world
            .query::<&Confirmation>()
            .single(world)
            .cloned()
            .unwrap()
    };
    assert_eq!(action, Confirmation::KillWorkspace("alpha".into()));
    assert_eq!(row_text(&app, 22), "kill workspace alpha? y/n");
    press(&mut app, TKey::Escape, Modifiers::NONE);
    assert!(popups(&mut app).is_empty());
    assert!(take_requests(&mut app).is_empty());
}

#[test]
fn prompts_edit_locally_and_commit_over_brp() {
    let server = common::Server::start();
    let list = server
        .call("fux/workspace.list", serde_json::json!({}))
        .unwrap();
    let root = list["workspaces"][0]["roots"][0]["id"].as_u64().unwrap();
    let scene = ServerScene::grid(1, 1);
    let mut app = viewer::build(80, 24);
    app.insert_resource(Brp::new(server.brp.clone(), std::sync::Arc::new(|| {})));
    app.world_mut()
        .resource_mut::<Inbox>()
        .frames
        .push(welcome("default"));
    push_frame(
        &mut app,
        SceneFrame {
            roots: Some(vec![RootEntry {
                node: NodeId(root),
                name: "main".into(),
            }]),
            showing: Some(NodeId(root)),
            ..scene.frame(1)
        },
    );
    app.update();
    take_requests(&mut app);

    chord(&mut app, ',');
    assert_eq!(mode(&app), Mode::Prompt);
    let prompt = |app: &mut App| {
        let world = app.world_mut();
        world
            .query::<&Prompt>()
            .single(world)
            .map(|p| (p.text().to_owned(), p.cursor()))
            .unwrap()
    };
    assert_eq!(
        prompt(&mut app),
        ("main".into(), 4),
        "starts from the current name"
    );
    assert_eq!(row_text(&app, 22), "rename: main");
    assert_eq!(
        app.world().resource::<Painter>().cursor(),
        Some((12, 22)),
        "the terminal cursor sits after the text"
    );
    // Motions and word edits: Home, Alt-f, Backspace, Ctrl-w, typed text.
    press(&mut app, TKey::Home, Modifiers::NONE);
    type_str(&mut app, "the ");
    assert_eq!(prompt(&mut app), ("the main".into(), 4));
    press(&mut app, TKey::End, Modifiers::NONE);
    press(&mut app, TKey::Char('w'), Modifiers::CONTROL);
    assert_eq!(prompt(&mut app), ("the ".into(), 4), "word delete");
    press(&mut app, TKey::Left, Modifiers::NONE);
    press(&mut app, TKey::Backspace, Modifiers::NONE);
    assert_eq!(prompt(&mut app), ("th ".into(), 2));
    press(&mut app, TKey::Char('f'), Modifiers::ALT);
    assert_eq!(prompt(&mut app), ("th ".into(), 3), "Alt-f to the word end");
    // Bracketed paste lands as one insert, control characters dropped.
    push_keys(&mut app, [Event::Paste("work\nspace".into())]);
    app.update();
    assert_eq!(prompt(&mut app), ("th workspace".into(), 12));
    press(&mut app, TKey::Char('a'), Modifiers::CONTROL);
    press(&mut app, TKey::Delete, Modifiers::NONE);
    press(&mut app, TKey::Delete, Modifiers::NONE);
    press(&mut app, TKey::Delete, Modifiers::NONE);
    assert_eq!(prompt(&mut app), ("workspace".into(), 0));
    assert_eq!(row_text(&app, 22), "rename: workspace");
    assert!(take_requests(&mut app).is_empty(), "editing sends nothing");

    // Escape cancels: nothing reaches the server.
    press(&mut app, TKey::Escape, Modifiers::NONE);
    assert!(popups(&mut app).is_empty());
    assert_eq!(mode(&app), Mode::Normal);
    chord(&mut app, ',');
    assert_eq!(prompt(&mut app).0, "main");

    // Enter commits `fux/root.rename` through the viewer's BRP worker.
    press(&mut app, TKey::End, Modifiers::NONE);
    type_str(&mut app, "-renamed");
    press(&mut app, TKey::Enter, Modifiers::NONE);
    assert!(popups(&mut app).is_empty());
    assert!(
        settle(&mut app, |_| {
            let list = server
                .call("fux/workspace.list", serde_json::json!({}))
                .unwrap();
            list["workspaces"][0]["roots"][0]["name"] == "main-renamed"
        }),
        "the server applied the rename"
    );
    assert!(
        take_requests(&mut app).is_empty(),
        "no ViewerRequest is involved"
    );

    // `prefix S` creates a workspace and attaches to it.
    chord(&mut app, 'S');
    assert_eq!(row_text(&app, 22), "new workspace:");
    type_str(&mut app, "beta");
    press(&mut app, TKey::Enter, Modifiers::NONE);
    assert!(
        settle(&mut app, |app| app
            .world()
            .get_resource::<Reconnect>()
            .is_some()),
        "a created workspace is attached to"
    );
    assert_eq!(
        app.world().get_resource::<Reconnect>(),
        Some(&Reconnect("beta".into()))
    );
    assert!(server.workspace_names().contains(&"beta".to_owned()));
    app.world_mut().remove_resource::<Reconnect>();

    // An empty name is refused locally; a server refusal becomes a notice.
    chord(&mut app, 'S');
    press(&mut app, TKey::Enter, Modifiers::NONE);
    assert!(popups(&mut app).is_empty());
    assert_eq!(row_text(&app, 22), "empty name");
    chord(&mut app, 'S');
    type_str(&mut app, "beta");
    press(&mut app, TKey::Enter, Modifiers::NONE);
    assert!(
        settle(&mut app, |app| row_text(app, 22)
            .starts_with("workspace beta:")),
        "duplicate workspace: {:?}",
        row_text(&app, 22)
    );
    assert!(app.world().get_resource::<Reconnect>().is_none());
}

#[test]
fn copy_mode_selects_searches_and_yanks_over_history() {
    let scene = ServerScene::grid(1, 1);
    let mut app = viewer::build(80, 24);
    app.insert_resource(ClipboardPolicy::WriteOnly);
    let mut frame = scene.frame(1);
    let mut screen = delta(1, 80, 23, "hello world");
    screen.lines.push(fux::wire::Line {
        row: 1,
        cells: "foo bar foo"
            .chars()
            .map(|c| Cell {
                text: String::from(c),
                width: 1,
                style: Style::default(),
            })
            .collect(),
    });
    screen.cursor = Cursor {
        row: 2,
        col: 0,
        visible: true,
    };
    frame.terminals = vec![screen];
    push_frame(&mut app, frame);
    app.update();
    take_requests(&mut app);
    let out = |app: &mut App| core::mem::take(&mut app.world_mut().resource_mut::<Painter>().out);
    out(&mut app);
    let view = |app: &mut App| {
        let world = app.world_mut();
        world
            .query::<&CopyView>()
            .single(world)
            .map(|v| (v.offset(), v.cursor(), v.selection(), v.history_len()))
            .unwrap()
    };

    chord(&mut app, '[');
    assert_eq!(mode(&app), Mode::CopyMode);
    assert_eq!(
        view(&mut app),
        (0, (2, 0), None, 0),
        "starts at the pane cursor"
    );
    assert!(row_text(&app, 23).contains("COPY"));
    // Search: `/foo` Enter lands on the first match, n/N walk them with wrap.
    type_str(&mut app, "/foo");
    assert!(
        row_text(&app, 23).contains("/foo"),
        "the query shows while typing"
    );
    press(&mut app, TKey::Enter, Modifiers::NONE);
    assert_eq!(view(&mut app).1, (1, 0));
    assert_ne!(
        cell_attrs(&app, 8, 1) & Style::UNDERLINE,
        0,
        "matches are underlined"
    );
    assert_eq!(cell_attrs(&app, 4, 1) & Style::UNDERLINE, 0);
    press(&mut app, TKey::Char('n'), Modifiers::NONE);
    assert_eq!(view(&mut app).1, (1, 8));
    press(&mut app, TKey::Char('n'), Modifiers::NONE);
    assert_eq!(view(&mut app).1, (1, 0), "wraps");
    press(&mut app, TKey::Char('N'), Modifiers::NONE);
    assert_eq!(view(&mut app).1, (1, 8));
    assert_eq!(
        app.world().resource::<Painter>().cursor(),
        Some((8, 1)),
        "the terminal cursor follows the copy cursor"
    );

    // Selection with v + motions, painted inverse; y copies it as OSC 52 and leaves.
    press(&mut app, TKey::Char('0'), Modifiers::NONE);
    press(&mut app, TKey::Char('v'), Modifiers::NONE);
    press(&mut app, TKey::Char('l'), Modifiers::NONE);
    press(&mut app, TKey::Char('l'), Modifiers::NONE);
    press(&mut app, TKey::Char('k'), Modifiers::NONE);
    assert_eq!(view(&mut app).2, Some(((0, 2), (1, 0))));
    assert_ne!(cell_attrs(&app, 2, 0) & Style::INVERSE, 0);
    assert_ne!(cell_attrs(&app, 10, 0) & Style::INVERSE, 0);
    assert_ne!(cell_attrs(&app, 0, 1) & Style::INVERSE, 0);
    assert_eq!(cell_attrs(&app, 1, 1) & Style::INVERSE, 0);
    assert_eq!(cell_attrs(&app, 1, 0) & Style::INVERSE, 0);
    out(&mut app);
    press(&mut app, TKey::Char('y'), Modifiers::NONE);
    let bytes = out(&mut app);
    // "llo world\nf"
    let osc = b"\x1b]52;c;bGxvIHdvcmxkCmY=\x07";
    assert!(
        bytes.windows(osc.len()).any(|w| w == osc),
        "the selection reaches the terminal clipboard: {:?}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(mode(&app), Mode::Normal);
    assert!(
        app.world_mut()
            .query::<&CopyView>()
            .iter(app.world())
            .next()
            .is_none(),
        "yank leaves copy mode"
    );
    assert!(
        take_requests(&mut app).is_empty(),
        "copy mode is viewer-local"
    );

    // History arrives from the capture worker and is prepended without moving the view.
    chord(&mut app, '[');
    let lines: Vec<String> = ["older", "old foo", "hello world", "foo bar foo"]
        .into_iter()
        .map(String::from)
        .chain(std::iter::repeat_n(String::new(), 21))
        .collect();
    app.world().resource::<Brp>().deliver(BrpReply {
        tag: BrpTag::Capture(PaneId(1)),
        result: Ok(serde_json::json!({
            "pane": 1, "seq": 1, "rows": 23, "cols": 80, "title": "", "state": "live",
            "cursor": { "row": 0, "col": 0, "visible": true },
            "lines": lines,
            "truncated": false
        })),
    });
    app.update();
    assert_eq!(
        view(&mut app),
        (2, (4, 0), None, 2),
        "two history rows above the screen"
    );
    assert_eq!(row_text(&app, 0), "hello world", "the view did not move");
    press(&mut app, TKey::Char('k'), Modifiers::NONE);
    press(&mut app, TKey::Char('k'), Modifiers::NONE);
    press(&mut app, TKey::Char('k'), Modifiers::NONE);
    assert_eq!(
        view(&mut app).0,
        1,
        "scrolled the minimum to show the cursor row"
    );
    assert_eq!(row_text(&app, 0), "old foo");
    assert_eq!(row_text(&app, 1), "hello world");
    press(&mut app, TKey::Char('g'), Modifiers::NONE);
    assert_eq!(row_text(&app, 0), "older");
    type_str(&mut app, "/foo");
    press(&mut app, TKey::Enter, Modifiers::NONE);
    assert_eq!(view(&mut app).1, (1, 4), "search covers history");
    press(&mut app, TKey::Char('G'), Modifiers::NONE);
    assert_eq!(view(&mut app).0, 2, "back to the live screen");
    press(&mut app, TKey::Char('q'), Modifiers::NONE);
    assert_eq!(mode(&app), Mode::Normal);
    assert_eq!(row_text(&app, 0), "hello world");
    assert_eq!(cell_attrs(&app, 8, 1), 0, "highlights are gone");
}

#[test]
fn focus_ring_returns_to_the_previous_pane() {
    let scene = ServerScene::grid(2, 2);
    let mut app = viewer::build(80, 24);
    push_frame(&mut app, scene.frame(1));
    app.update();
    let leaf = |app: &App, i: usize| local(app, scene.leaves[i]);
    chord(&mut app, 'l');
    chord(&mut app, 'j');
    assert_eq!(focused(&app), Some(leaf(&app, 3)));
    take_requests(&mut app);
    chord(&mut app, ';');
    assert_eq!(
        focused(&app),
        Some(leaf(&app, 1)),
        "back to the previous pane"
    );
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::Target(PaneId(2))]
    );
    chord(&mut app, ';');
    assert_eq!(focused(&app), Some(leaf(&app, 3)), "and forth");
    let ring: Vec<NodeId> = app.world().resource::<FocusRing>().entries().collect();
    assert_eq!(ring.len(), 3, "each leaf once, most recent last: {ring:?}");
    assert!(ring.len() <= FocusRing::CAPACITY);

    // A leaf that disappeared is skipped.
    let mut gone = scene.frame(2);
    gone.scene = String::new();
    gone.despawned = vec![scene.leaves[1].to_bits()];
    gone.roots = None;
    push_frame(&mut app, gone);
    app.update();
    take_requests(&mut app);
    chord(&mut app, ';');
    assert_eq!(
        focused(&app),
        Some(leaf(&app, 0)),
        "leaf 1 is gone: the one before it"
    );
}

#[test]
fn focus_on_a_surface_leaf_routes_keys_to_surface_key_not_input() {
    let scene = ServerScene::beside_surface();
    let mut app = viewer::build(80, 24);
    push_frame(&mut app, scene.frame(1));
    app.update();
    let pane_leaf = local(&app, scene.leaves[0]);
    let surface_leaf = local(&app, scene.leaves[1]);
    assert_eq!(focused(&app), Some(pane_leaf));
    assert!(
        app.world().get::<TabIndex>(surface_leaf).is_some(),
        "a surface leaf is in the tab order"
    );
    take_requests(&mut app);

    // `prefix l` lands on the surface leaf and retargets nothing: it shows no pane.
    chord(&mut app, 'l');
    assert_eq!(focused(&app), Some(surface_leaf));
    assert_eq!(take_requests(&mut app), vec![]);

    // Keys leave as `SurfaceKey` for node 3 with their terminal bytes, never as pane input.
    press(&mut app, TKey::Char('x'), Modifiers::NONE);
    press(&mut app, TKey::Up, Modifiers::NONE);
    press(&mut app, TKey::Char('c'), Modifiers::CONTROL);
    assert_eq!(
        take_requests(&mut app),
        vec![
            ViewerRequest::SurfaceKey {
                revision: 1,
                node: NodeId(3),
                bytes: b"x".to_vec()
            },
            ViewerRequest::SurfaceKey {
                revision: 1,
                node: NodeId(3),
                bytes: b"\x1b[A".to_vec()
            },
            ViewerRequest::SurfaceKey {
                revision: 1,
                node: NodeId(3),
                bytes: b"\x03".to_vec()
            },
        ]
    );
    // The prefix still opens a chord, and the focus stays where it was for the rest.
    chord(&mut app, 'h');
    assert_eq!(focused(&app), Some(pane_leaf));
    assert_eq!(
        take_requests(&mut app),
        vec![],
        "back on the server's target: nothing to request"
    );
    press(&mut app, TKey::Char('y'), Modifiers::NONE);
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::Input(b"y".to_vec())]
    );

    // A click on the streamed text row focuses the surface leaf above it; the server's
    // target does not pull the focus back while it stays unchanged.
    assert_eq!(click(&mut app, 60, 0), vec![]);
    assert_eq!(focused(&app), Some(surface_leaf));
    app.update();
    app.update();
    assert_eq!(focused(&app), Some(surface_leaf));
    press(&mut app, TKey::Enter, Modifiers::NONE);
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::SurfaceKey {
            revision: 1,
            node: NodeId(3),
            bytes: b"\r".to_vec()
        }]
    );
    // `prefix ;` returns through the ring to the pane and back to the surface.
    chord(&mut app, ';');
    assert_eq!(focused(&app), Some(pane_leaf));
    chord(&mut app, ';');
    assert_eq!(focused(&app), Some(surface_leaf));
}

#[test]
fn surface_focus_survives_instance_replacement_before_the_next_key() {
    let mut scene = ServerScene::beside_surface();
    let mut app = viewer::build(80, 24);
    push_frame(&mut app, scene.frame(1));
    app.update();
    chord(&mut app, 'l');
    take_requests(&mut app);
    let old = scene.leaves[1];
    let child = scene
        .world
        .get::<Children>(old)
        .unwrap()
        .iter()
        .next()
        .unwrap();
    let node = scene.world.get::<Node>(old).unwrap().clone();
    scene.world.entity_mut(old).despawn();
    let replacement = scene
        .world
        .spawn((InstanceNode, NodeId(3), ChildOf(scene.root), Surface, node))
        .id();
    scene.world.spawn((
        InstanceNode,
        NodeId(4),
        ChildOf(replacement),
        SurfaceText("selected task".into()),
        Node {
            height: Val::Px(1.0),
            ..Default::default()
        },
    ));
    scene.leaves[1] = replacement;
    let mut frame = scene.frame(2);
    frame.despawned = vec![old.to_bits(), child.to_bits()];
    push_frame(&mut app, frame);
    app.update();
    press(&mut app, TKey::Char('s'), Modifiers::NONE);
    assert_eq!(
        take_requests(&mut app),
        vec![ViewerRequest::SurfaceKey {
            revision: 2,
            node: NodeId(3),
            bytes: b"s".to_vec()
        }]
    );
}

#[test]
fn surface_labels_cannot_emit_outer_terminal_control_sequences() {
    let mut scene = ServerScene::beside_surface();
    for mut text in scene
        .world
        .query::<&mut SurfaceText>()
        .iter_mut(&mut scene.world)
    {
        text.0 = "label \u{1b}]52;c;c2VjcmV0\u{7}".into();
    }
    let mut app = viewer::build(80, 24);
    push_frame(&mut app, scene.frame(1));
    app.update();
    let output = &app.world().resource::<Painter>().out;
    assert!(output.windows(5).any(|bytes| bytes == b"label"));
    assert!(!output.windows(5).any(|bytes| bytes == b"\x1b]52;"));
    assert!(!output.contains(&7));
}
