//! Configuration as an asset (milestone 5, prompt 3.7): a headless server over a temporary
//! config directory loads `fux.toml` through the asset server, follows edits through the file
//! watcher without touching panes, keeps the previous values on an invalid edit, and derives
//! the theme and keybinding sub-assets from the same file.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy_app::App;
use bevy_asset::{AssetServer, Assets, Handle, LoadState};
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_ecs::query::Allow;
use tempfile::TempDir;

use fux::app::build_headless_in;
use fux::assets::{
    BINDINGS_PATH, ConfigAsset, ConfigHandle, Keybindings, THEME_PATH, Theme, ThemeToken,
    describe_bindings,
};
use fux::config::Config;
use fux::lifecycle::{self, Clock, DefaultCommand};
use fux::model::{Inbound, Limits, Pane, Process};
use fux::paths::Paths;
use fux::viewer::keys::KeyChord;
use fux::wire::Color;

/// How long an edit may take to reach the World: the watcher debounces 300 ms.
const RELOAD_WINDOW: Duration = Duration::from_secs(2);

struct ConfigDir {
    _dir: TempDir,
    paths: Paths,
}

impl ConfigDir {
    /// A private config directory (canonical: the file watcher reports canonical paths).
    fn new(document: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let paths = Paths {
            runtime_dir: root.join("run"),
            config_dir: root.join("config"),
            state_dir: root.join("state"),
        };
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let this = Self { _dir: dir, paths };
        this.write(document);
        this
    }

    fn file(&self) -> PathBuf {
        self.paths.config_file()
    }

    fn write(&self, document: &str) {
        std::fs::write(self.file(), document).unwrap();
    }

    /// A server over this directory, started from the file as `Config::load` reads it.
    fn server(&self) -> App {
        let config = Config::load(&self.file()).unwrap();
        let mut app = build_headless_in(&config, &self.paths);
        app.world_mut().resource_mut::<Clock>().now_ms = 1_000;
        app
    }
}

/// Steps the app until `done` holds or the window elapses.
fn settle(app: &mut App, window: Duration, mut done: impl FnMut(&mut App) -> bool) -> bool {
    let start = Instant::now();
    loop {
        app.update();
        if done(app) {
            return true;
        }
        if start.elapsed() > window {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn limits(app: &App) -> Limits {
    app.world().resource::<Limits>().clone()
}

fn panes(app: &mut App) -> Vec<(Entity, Process)> {
    app.world_mut()
        .query_filtered::<(Entity, &Process), (With<Pane>, Allow<Disabled>)>()
        .iter(app.world())
        .map(|(e, p)| (e, *p))
        .collect()
}

fn loaded(app: &App) -> bool {
    let handle = app.world().resource::<ConfigHandle>();
    app.world()
        .resource::<Assets<ConfigAsset>>()
        .contains(&handle.0)
}

// ---------------------------------------------------------------------------------------------
// (1) initial load and hot reload on the server
// ---------------------------------------------------------------------------------------------

#[test]
fn config_asset_applies_limits_and_follows_edits_without_touching_panes() {
    let dir =
        ConfigDir::new("default-command = { argv = ['/bin/sh', '-l'] }\n[limits]\nmax-panes = 7\n");
    // Start from the defaults on purpose: the asset load must apply the file.
    let mut app = build_headless_in(&Config::default(), &dir.paths);
    app.world_mut().resource_mut::<Clock>().now_ms = 1_000;
    assert_eq!(
        limits(&app).panes_per_workspace,
        Limits::default().panes_per_workspace
    );
    assert!(
        settle(&mut app, RELOAD_WINDOW, |app| loaded(app)),
        "fux.toml loads"
    );
    assert_eq!(limits(&app).panes_per_workspace, 7, "[limits] applied");
    assert_eq!(
        app.world().resource::<DefaultCommand>().0,
        vec!["/bin/sh".to_owned(), "-l".to_owned()],
        "default-command applied"
    );
    assert_eq!(app.world().resource::<Config>().limits.max_panes, 7);

    // A live pane.
    lifecycle::bootstrap(app.world_mut(), "default", &["/bin/sh".into()]).unwrap();
    app.update();
    let (pane, _) = panes(&mut app)[0];
    app.world_mut()
        .resource_mut::<Messages<Inbound>>()
        .write(Inbound::PaneSpawned { pane, pid: 42 });
    app.update();
    assert_eq!(panes(&mut app), vec![(pane, Process::Live { pid: 42 })]);

    // Edit: the new limit lands within the window; the pane is untouched.
    dir.write("[limits]\nmax-panes = 9\nmax-nodes = 40\n");
    assert!(
        settle(&mut app, RELOAD_WINDOW, |app| limits(app)
            .panes_per_workspace
            == 9),
        "the edited limit is applied within {RELOAD_WINDOW:?}; got {:?}",
        limits(&app)
    );
    assert_eq!(limits(&app).nodes_per_workspace, 40);
    assert_eq!(
        app.world().resource::<DefaultCommand>().0,
        vec![Config::default().default_command.argv[0].clone()],
        "a key dropped from the file falls back to its default"
    );
    assert_eq!(
        panes(&mut app),
        vec![(pane, Process::Live { pid: 42 })],
        "the pane entity and its process survive a reload"
    );

    // An invalid edit keeps the previous values and the previous asset.
    dir.write("[limits]\nmax-panes = 3\nbogus = 1\n");
    let handle = app.world().resource::<ConfigHandle>().0.clone();
    assert!(
        settle(&mut app, RELOAD_WINDOW, |app| matches!(
            app.world()
                .resource::<AssetServer>()
                .get_load_state(&handle),
            Some(LoadState::Failed(_))
        )),
        "the invalid edit is refused"
    );
    app.update();
    assert_eq!(
        limits(&app).panes_per_workspace,
        9,
        "unknown key: previous Limits kept"
    );
    assert_eq!(app.world().resource::<Config>().limits.max_panes, 9);
    assert_eq!(
        app.world()
            .resource::<Assets<ConfigAsset>>()
            .get(&handle)
            .map(|c| c.0.limits.max_panes),
        Some(9),
        "the previous asset stays loaded"
    );

    dir.write("[limits]\nmax-panes = 0\n");
    std::thread::sleep(Duration::from_millis(500));
    settle(&mut app, Duration::from_millis(500), |_| false);
    assert_eq!(
        limits(&app).panes_per_workspace,
        9,
        "bad value: previous Limits kept"
    );
    assert_eq!(
        panes(&mut app),
        vec![(pane, Process::Live { pid: 42 })],
        "refused reloads touch nothing"
    );
}

#[test]
fn missing_config_means_defaults() {
    let dir = ConfigDir::new("");
    std::fs::remove_file(dir.file()).unwrap();
    let mut app = dir.server();
    let handle = app.world().resource::<ConfigHandle>().0.clone();
    assert!(
        settle(&mut app, RELOAD_WINDOW, |app| matches!(
            app.world()
                .resource::<AssetServer>()
                .get_load_state(&handle),
            Some(LoadState::Failed(_))
        )),
        "the missing file is reported"
    );
    app.update();
    assert_eq!(limits(&app), Limits::default());
    assert_eq!(*app.world().resource::<Config>(), Config::default());
}

// ---------------------------------------------------------------------------------------------
// (2) theme and keybinding sub-assets
// ---------------------------------------------------------------------------------------------

#[test]
fn theme_and_bindings_are_sub_assets_of_the_same_file() {
    let dir = ConfigDir::new(
        "prefix = 'C-a'\n[bindings]\n'|' = 'split-side'\n[style]\nbar-background = 'blue'\n\
         focus-ring = 'bright-white'\n",
    );
    let mut app = dir.server();
    let theme: Handle<Theme> = app.world().resource::<AssetServer>().load(THEME_PATH);
    let bindings: Handle<Keybindings> = app.world().resource::<AssetServer>().load(BINDINGS_PATH);
    assert!(
        settle(&mut app, RELOAD_WINDOW, |app| {
            app.world().resource::<Assets<Theme>>().contains(&theme)
                && app
                    .world()
                    .resource::<Assets<Keybindings>>()
                    .contains(&bindings)
        }),
        "both labeled assets load"
    );
    let loaded_theme = app.world().resource::<Assets<Theme>>().get(&theme).unwrap();
    assert_eq!(
        loaded_theme.color(ThemeToken::BAR_BACKGROUND),
        Color::Indexed(4)
    );
    assert_eq!(
        loaded_theme.color(ThemeToken::FOCUS_RING),
        Color::Indexed(15)
    );
    let loaded_bindings = app
        .world()
        .resource::<Assets<Keybindings>>()
        .get(&bindings)
        .unwrap();
    assert_eq!(loaded_bindings.prefix, KeyChord::ctrl('a'));
    assert_eq!(
        loaded_bindings
            .bindings
            .get(&KeyChord::character('|'))
            .map(|action| action.as_ref()),
        Some("split-side")
    );

    // The edited theme replaces the sub-asset in place.
    dir.write("prefix = 'C-c'\n[style]\nbar-background = 'red'\n");
    assert!(
        settle(&mut app, RELOAD_WINDOW, |app| {
            app.world()
                .resource::<Assets<Theme>>()
                .get(&theme)
                .is_some_and(|t| t.color(ThemeToken::BAR_BACKGROUND) == Color::Indexed(1))
        }),
        "the theme follows the edit"
    );
    let loaded_bindings = app
        .world()
        .resource::<Assets<Keybindings>>()
        .get(&bindings)
        .unwrap();
    assert_eq!(
        loaded_bindings.prefix,
        KeyChord::ctrl('c'),
        "the prefix follows the edit too"
    );
}

#[test]
fn an_unported_action_name_keeps_the_rest_of_the_file() {
    // `new-tab` is in today's registry but not in this build's `ACTIONS`.
    let dir = ConfigDir::new(
        "prefix = 'C-a'\n[bindings]\n'|' = 'split-side'\n't' = 'new-tab'\n[limits]\nmax-panes = 7\n",
    );
    let mut app = build_headless_in(&Config::default(), &dir.paths);
    let bindings: Handle<Keybindings> = app.world().resource::<AssetServer>().load(BINDINGS_PATH);
    assert!(
        settle(&mut app, RELOAD_WINDOW, |app| {
            loaded(app)
                && app
                    .world()
                    .resource::<Assets<Keybindings>>()
                    .contains(&bindings)
        }),
        "the file loads despite the unknown action"
    );
    assert_eq!(limits(&app).panes_per_workspace, 7, "[limits] applied");
    let loaded_bindings = app
        .world()
        .resource::<Assets<Keybindings>>()
        .get(&bindings)
        .unwrap();
    assert_eq!(loaded_bindings.prefix, KeyChord::ctrl('a'));
    assert_eq!(
        loaded_bindings
            .bindings
            .get(&KeyChord::character('|'))
            .map(|action| action.as_ref()),
        Some("split-side")
    );
    assert_eq!(
        loaded_bindings.bindings.get(&KeyChord::character('t')),
        None,
        "the unknown action binds nothing"
    );
}

// ---------------------------------------------------------------------------------------------
// (3) chord text and the `fux bindings` listing
// ---------------------------------------------------------------------------------------------

#[test]
fn chord_text_round_trips() {
    for (text, canonical) in [
        ("C-x", "C-x"),
        ("M-x", "M-x"),
        ("C-M-x", "C-M-x"),
        ("Esc", "Esc"),
        ("escape", "Esc"),
        ("Space", "Space"),
        ("C-Space", "C-Space"),
        ("a", "a"),
        ("|", "|"),
        ("-", "-"),
        ("Tab", "Tab"),
        ("S-Tab", "S-Tab"),
        ("BackTab", "S-Tab"),
        ("DEL", "Backspace"),
        ("PgUp", "PageUp"),
        ("F5", "F5"),
        ("0x01", "C-a"),
        ("0x1b", "Esc"),
        ("0x7f", "Backspace"),
        ("0x25", "%"),
    ] {
        let chord = KeyChord::parse(text).unwrap_or_else(|| panic!("{text} parses"));
        assert_eq!(chord.to_string(), canonical, "{text}");
        assert_eq!(
            KeyChord::parse(canonical),
            Some(chord),
            "{canonical} re-parses"
        );
    }
    for text in ["", "C-", "ab", "S-a", "C-Esc", "Hyper", "0x80", "0xzz", " "] {
        assert_eq!(KeyChord::parse(text), None, "{text:?} is refused");
    }
    let bytes = |text: &str| {
        let mut out = Vec::new();
        assert!(KeyChord::parse(text).unwrap().bytes(&mut out), "{text}");
        out
    };
    assert_eq!(bytes("C-b"), b"\x02");
    assert_eq!(bytes("M-x"), b"\x1bx");
    assert_eq!(bytes("Esc"), b"\x1b");
    assert_eq!(bytes("S-Tab"), b"\x1b[Z");
    assert_eq!(bytes("C-Space"), b"\x00");
    assert_eq!(bytes("%"), b"%");
}

#[test]
fn bindings_listing_shows_the_effective_prefix_and_table() {
    let config = Config::from_toml("prefix = 'C-a'\n[bindings]\n'|' = 'split-side'\n").unwrap();
    let listing = describe_bindings(&config).unwrap();
    let lines: Vec<&str> = listing.lines().collect();
    assert_eq!(lines[0], "prefix C-a");
    assert!(lines.contains(&"| split-side"), "{listing}");
    assert!(lines.contains(&"% split-side"), "{listing}");
    assert!(lines.contains(&"C-a send-prefix"), "{listing}");
    assert!(lines.contains(&"S-Tab prev-pane"), "{listing}");
    // The prefix line, the defaults (whose `C-b send-prefix` became `C-a`), plus `|`.
    assert_eq!(lines.len(), 1 + Keybindings::default().bindings.len() + 1);
}
