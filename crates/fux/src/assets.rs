//! User-editable assets (prompt 3.7): `fux.toml` is a `bevy_asset` [`Asset`] loaded by
//! [`ConfigLoader`] from the config directory and watched by `file_watcher`. One load produces
//! the [`ConfigAsset`] and two labeled sub-assets, `fux.toml#theme` ([`Theme`], the `[style]`
//! tokens) and `fux.toml#bindings` ([`Keybindings`], the prefix and `[bindings]` merged over the
//! defaults). The server applies `Limits`, `DefaultCommand` and the `Config` resource from
//! `AssetEvent`s; the viewer resolves `ThemeToken`s and rebuilds its chord table from the same
//! file through its own `AssetPlugin`. A reload that fails to parse or validate leaves the
//! previous asset in place (bevy keeps it) and is logged; a missing file means defaults.

use core::pin::Pin;
use core::task::{Context, Poll};
use core::time::Duration;
use std::path::Path;
use std::sync::Arc;

use async_channel::Sender;
use bevy_app::prelude::*;
use bevy_asset::io::{
    AssetReaderError, AssetSource, AssetSourceBuilder, ErasedAssetReader, PathStream, Reader,
    ReaderNotSeekableError, SeekableReader,
};
use bevy_asset::{
    Asset, AssetApp, AssetEvent, AssetEventSystems, AssetLoadError, AssetLoadFailedEvent,
    AssetLoader, AssetServer, Assets, Handle, LoadContext,
};
use bevy_ecs::prelude::*;
use bevy_log::{info, warn};
use bevy_platform::collections::HashMap;
use bevy_reflect::TypePath;
use bevy_tasks::futures_lite::AsyncRead;
use bevy_tasks::{BoxedFuture, IoTaskPool};

use crate::config::{Config, ConfigError};
use crate::lifecycle::DefaultCommand;
use crate::model::{Inbound, Limits};
use crate::viewer::focus::all_actions;
use crate::viewer::keys::KeyChord;
use crate::wire::Color as WireColor;

/// The configuration file, relative to the config directory the `AssetPlugin` serves.
pub const CONFIG_FILE: &str = "fux.toml";
/// The theme sub-asset path.
pub const THEME_PATH: &str = "fux.toml#theme";
/// The keybindings sub-asset path.
pub const BINDINGS_PATH: &str = "fux.toml#bindings";

/// The `AssetPlugin::file_path` for a config directory: canonical when it exists, because the
/// file watcher reports canonical paths and refuses events it cannot strip its root from
/// (macOS `/var` is `/private/var`; a symlinked `$XDG_CONFIG_HOME` likewise).
pub fn asset_root(config_dir: &Path) -> String {
    config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_owned())
        .display()
        .to_string()
}

/// What an asset source calls to wake a blocked runner; `Arc<dyn Fn>` so the reader, the
/// watcher task and every file reader share one.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// The wake for the server runner: an `Inbound::Wake` (a full channel means the runner is
/// already awake and stepping).
pub fn inbound_wake(inbound: Sender<Inbound>) -> Wake {
    Arc::new(move || {
        let _ = inbound.try_send(Inbound::Wake);
    })
}

/// A default asset source: bevy's file reader and watcher over `root`, except that every
/// watcher event and every completed read calls `wake` (prompt 3.6: task-pool activity becomes
/// a runner wake-up). Both are needed: a change event is turned into a reload by
/// `handle_internal_asset_events`, which only runs inside an update, and the reload's result is
/// applied by the update after the read. Insert under `AssetSourceId::Default` before
/// `AssetPlugin`; needs the task pools.
pub fn waking_source(root: &str, wake: Wake) -> AssetSourceBuilder {
    let mut inner = AssetSource::get_default_reader(root.to_owned());
    let mut watcher = AssetSource::get_default_watcher(root.to_owned(), WATCH_DEBOUNCE);
    let read_wake = Arc::clone(&wake);
    AssetSourceBuilder::platform_default(root, None)
        .with_reader(move || {
            Box::new(WakingAssetReader {
                inner: inner(),
                wake: Arc::clone(&read_wake),
            })
        })
        .with_watcher(move |sender| {
            let (proxy, events) = async_channel::unbounded();
            let watcher = watcher(proxy)?;
            let wake = Arc::clone(&wake);
            IoTaskPool::get()
                .spawn(async move {
                    while let Ok(event) = events.recv().await {
                        if sender.send(event).await.is_err() {
                            break;
                        }
                        wake();
                    }
                })
                .detach();
            Some(watcher)
        })
}

/// bevy's own file-watcher debounce.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);

struct WakingAssetReader {
    inner: Box<dyn ErasedAssetReader>,
    wake: Wake,
}

impl ErasedAssetReader for WakingAssetReader {
    fn read<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxedFuture<'a, Result<Box<dyn Reader + 'a>, AssetReaderError>> {
        Box::pin(async move {
            let inner = self.inner.read(path).await?;
            Ok(Box::new(WakingReader {
                inner,
                wake: Arc::clone(&self.wake),
            }) as Box<dyn Reader + 'a>)
        })
    }

    fn read_meta<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxedFuture<'a, Result<Box<dyn Reader + 'a>, AssetReaderError>> {
        self.inner.read_meta(path)
    }

    fn read_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxedFuture<'a, Result<Box<PathStream>, AssetReaderError>> {
        self.inner.read_directory(path)
    }

    fn is_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxedFuture<'a, Result<bool, AssetReaderError>> {
        self.inner.is_directory(path)
    }

    fn read_meta_bytes<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxedFuture<'a, Result<Vec<u8>, AssetReaderError>> {
        self.inner.read_meta_bytes(path)
    }
}

/// A file reader whose drop, which the asset server reaches only after it queued the load's
/// result, wakes the runner.
struct WakingReader<'a> {
    inner: Box<dyn Reader + 'a>,
    wake: Wake,
}

impl AsyncRead for WakingReader<'_> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl Reader for WakingReader<'_> {
    fn seekable(&mut self) -> Result<&mut dyn SeekableReader, ReaderNotSeekableError> {
        self.inner.seekable()
    }
}

impl Drop for WakingReader<'_> {
    fn drop(&mut self) {
        (self.wake)();
    }
}

// ---------------------------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------------------------

/// A theme slot by name, the `bevy_feathers` way: chrome entities carry the token, one resolve
/// pass per theme change turns it into the cell style the painter reads. The set is closed:
/// [`ThemeToken::ALL`] lists every token `[style]` may name.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[component(immutable)]
#[require(crate::viewer::theme::CellStyle)]
pub struct ThemeToken(pub &'static str);

impl ThemeToken {
    /// Text on the status bar (title, workspace name).
    pub const BAR: Self = Self("bar");
    /// Fill of the status bar row.
    pub const BAR_BACKGROUND: Self = Self("bar-background");
    /// Inactive tab entries.
    pub const TAB: Self = Self("tab");
    /// The shown root's tab entry and the mode indicator, drawn reversed on the bar.
    pub const TAB_ACTIVE: Self = Self("tab-active");
    /// Transient notices, drawn reversed.
    pub const NOTICE: Self = Self("notice");
    /// Border of a pane that is not focused and names no colour of its own.
    pub const PANE_BORDER: Self = Self("pane-border");
    /// Border of the focused pane.
    pub const FOCUS_RING: Self = Self("focus-ring");

    pub const ALL: [Self; 7] = [
        Self::BAR,
        Self::BAR_BACKGROUND,
        Self::TAB,
        Self::TAB_ACTIVE,
        Self::NOTICE,
        Self::PANE_BORDER,
        Self::FOCUS_RING,
    ];

    /// Today's colour for the token: the viewer's built-in look.
    pub fn default_color(self) -> WireColor {
        match self.0 {
            "bar" => WireColor::Rgb(220, 220, 220),
            "bar-background" => WireColor::Rgb(40, 40, 48),
            "tab" => WireColor::Rgb(150, 150, 160),
            "tab-active" => WireColor::Rgb(32, 110, 201),
            "notice" => WireColor::Rgb(217, 178, 51),
            "focus-ring" => WireColor::Indexed(14),
            _ => WireColor::Default,
        }
    }
}

/// The colours `[style]` names, keyed by token; every token has a value (the defaults fill in).
#[derive(Asset, TypePath, Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub colors: HashMap<ThemeToken, WireColor>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            colors: ThemeToken::ALL
                .iter()
                .map(|token| (*token, token.default_color()))
                .collect(),
        }
    }
}

impl Theme {
    pub fn color(&self, token: ThemeToken) -> WireColor {
        self.colors
            .get(&token)
            .copied()
            .unwrap_or_else(|| token.default_color())
    }
}

// ---------------------------------------------------------------------------------------------
// Keybindings
// ---------------------------------------------------------------------------------------------

/// The built-in prefix-mode table, chord text → action name.
pub const DEFAULT_BINDINGS: &[(&str, &str)] = &[
    ("%", "split-side"),
    ("\"", "split-below"),
    ("x", "close-pane"),
    ("h", "focus-left"),
    ("j", "focus-down"),
    ("k", "focus-up"),
    ("l", "focus-right"),
    ("o", "next-pane"),
    ("Tab", "next-pane"),
    ("S-Tab", "prev-pane"),
    (";", "previous-pane"),
    ("n", "next-root"),
    ("p", "prev-root"),
    ("c", "new-root"),
    ("w", "choose-tab"),
    (",", "rename-root"),
    ("s", "choose-workspace"),
    ("S", "new-workspace"),
    ("K", "kill-workspace"),
    ("d", "detach"),
    ("z", "zoom"),
    ("[", "copy-mode"),
    ("?", "help"),
];

/// A built-in action or explicitly namespaced plugin invocation.
pub fn action_name(name: &str) -> Option<std::borrow::Cow<'static, str>> {
    all_actions()
        .map(|(action, _)| *action)
        .find(|action| *action == name)
        .map(std::borrow::Cow::Borrowed)
        .or_else(|| plugin_action(name).map(|_| std::borrow::Cow::Owned(name.to_owned())))
}

/// The unambiguous `plugin:NAME/ACTION` configuration vocabulary.
pub fn plugin_action(name: &str) -> Option<(&str, &str)> {
    let (plugin, action) = name.strip_prefix("plugin:")?.split_once('/')?;
    let valid = |id: &str| {
        !id.is_empty()
            && id.len() <= 64
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    };
    (valid(plugin) && valid(action)).then_some((plugin, action))
}

/// The prefix chord and the prefix-mode table: action names by chord, user bindings merged over
/// [`DEFAULT_BINDINGS`] plus the prefix itself bound to `send-prefix`.
#[derive(Asset, TypePath, Clone, Debug, PartialEq, Eq)]
pub struct Keybindings {
    pub prefix: KeyChord,
    pub bindings: HashMap<KeyChord, std::borrow::Cow<'static, str>>,
}

impl Default for Keybindings {
    fn default() -> Self {
        Self::with_prefix(KeyChord::ctrl('b'))
    }
}

impl Keybindings {
    /// The default table under `prefix`.
    pub fn with_prefix(prefix: KeyChord) -> Self {
        let mut bindings: HashMap<KeyChord, std::borrow::Cow<'static, str>> = DEFAULT_BINDINGS
            .iter()
            .filter_map(|(chord, action)| Some((KeyChord::parse(chord)?, action_name(action)?)))
            .collect();
        bindings.insert(prefix.clone(), "send-prefix".into());
        Self { prefix, bindings }
    }

    /// The table sorted by chord text, for listing.
    pub fn sorted(&self) -> Vec<(String, &str)> {
        let mut rows: Vec<(String, &str)> = self
            .bindings
            .iter()
            .map(|(chord, action)| (chord.to_string(), action.as_ref()))
            .collect();
        rows.sort();
        rows
    }
}

/// `fux bindings`: the effective prefix and table for a configuration, one binding per line.
pub fn describe_bindings(config: &Config) -> Result<String, ConfigError> {
    let bindings = config.keybindings()?;
    let mut out = format!("prefix {}\n", bindings.prefix);
    for (chord, action) in bindings.sorted() {
        out.push_str(&chord);
        out.push(' ');
        out.push_str(action);
        out.push('\n');
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// The config asset and its loader
// ---------------------------------------------------------------------------------------------

/// `fux.toml`, parsed and validated.
#[derive(Asset, TypePath, Clone, Debug, PartialEq, Eq)]
pub struct ConfigAsset(pub Config);

/// Loads `*.toml` as [`ConfigAsset`] with the `theme` and `bindings` sub-assets.
#[derive(TypePath, Default)]
pub struct ConfigLoader;

impl AssetLoader for ConfigLoader {
    type Asset = ConfigAsset;
    type Settings = ();
    type Error = ConfigError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<ConfigAsset, ConfigError> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| ConfigError::Io(load_context.path().path().to_owned(), error))?;
        let config = Config::from_bytes(&bytes)?;
        load_context.add_labeled_asset("theme", config.theme());
        load_context.add_labeled_asset("bindings", config.keybindings()?);
        Ok(ConfigAsset(config))
    }

    fn extensions(&self) -> &[&str] {
        &["toml"]
    }
}

/// The handle every consumer watches: `fux.toml` in the served config directory.
#[derive(Resource, Debug, Clone)]
pub struct ConfigHandle(pub Handle<ConfigAsset>);

impl ConfigHandle {
    /// Whether an event names this file.
    pub fn changed(&self, event: &AssetEvent<ConfigAsset>) -> bool {
        matches!(event, AssetEvent::Added { id } | AssetEvent::Modified { id } if *id == self.0.id())
    }
}

/// Logs why a (re)load was refused; a missing file at startup is not an error. bevy attempts a
/// typed and an untyped reload of a changed path, so one bad edit fails twice: report it once.
pub fn report_config_failures(
    mut failures: MessageReader<AssetLoadFailedEvent<ConfigAsset>>,
    handle: Res<ConfigHandle>,
    configs: Res<Assets<ConfigAsset>>,
) {
    let Some(failure) = failures.read().filter(|f| f.id == handle.0.id()).last() else {
        return;
    };
    let missing = matches!(
        failure.error,
        AssetLoadError::AssetReaderError(AssetReaderError::NotFound(_))
    );
    if missing && !configs.contains(&handle.0) {
        info!("{CONFIG_FILE} not found: using the defaults");
    } else {
        warn!(
            "{CONFIG_FILE} not reloaded, keeping the previous configuration: {}",
            failure.error
        );
    }
}

/// Registers the assets and the loader and starts loading `fux.toml`; shared by the server and
/// the viewer. Needs `AssetPlugin` first.
pub struct ConfigAssetPlugin;

impl Plugin for ConfigAssetPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<ConfigAsset>()
            .init_asset::<Theme>()
            .init_asset::<Keybindings>()
            .register_asset_loader(ConfigLoader);
        let handle = app.world().resource::<AssetServer>().load(CONFIG_FILE);
        app.insert_resource(ConfigHandle(handle))
            .add_systems(PostUpdate, report_config_failures.after(AssetEventSystems));
    }
}

/// Server side: a loaded or reloaded `fux.toml` updates `Limits`, `DefaultCommand` and the
/// `Config` resource in place. Panes are never touched; `set_if_neq` keeps unchanged values
/// from marking anything changed.
fn apply_config(
    mut events: MessageReader<AssetEvent<ConfigAsset>>,
    handle: Res<ConfigHandle>,
    configs: Res<Assets<ConfigAsset>>,
    mut limits: ResMut<Limits>,
    mut default_command: ResMut<DefaultCommand>,
    mut current: ResMut<Config>,
) {
    if !events.read().any(|event| handle.changed(event)) {
        return;
    }
    let Some(ConfigAsset(config)) = configs.get(&handle.0) else {
        return;
    };
    limits.set_if_neq(config.limits());
    default_command.set_if_neq(DefaultCommand(config.default_command.argv.clone()));
    if current.set_if_neq(config.clone()) {
        info!("{CONFIG_FILE} applied");
    }
}

/// The server's configuration plugin: [`ConfigAssetPlugin`] plus the resources it drives.
pub struct ServerConfigPlugin;

impl Plugin for ServerConfigPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ConfigAssetPlugin)
            .add_systems(PostUpdate, apply_config.after(AssetEventSystems));
    }
}
