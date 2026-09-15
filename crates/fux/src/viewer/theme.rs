//! Theme resolution (prompt 3.7, the `bevy_feathers` manner): chrome entities carry an
//! immutable [`ThemeToken`]; one pass per theme change (`AssetEvent<Theme>` on the viewer's
//! `fux.toml#theme` handle) or per newly tokened entity (`Changed<ThemeToken>`) writes the
//! resolved [`CellStyle`] the painter reads, plus the [`BorderStyles`] for replicated leaves,
//! which carry no token. No colour is stored on an entity by hand.

use bevy_app::prelude::*;
use bevy_asset::{AssetEvent, AssetEventSystems, AssetServer, Assets, Handle};
use bevy_ecs::prelude::*;

use crate::assets::{THEME_PATH, Theme, ThemeToken};
use crate::wire::{Color as WireColor, Style};

/// The resolved style of a chrome node: `bg` is the node's fill (`Default` keeps whatever is
/// underneath), `fg`/`attrs` style its text. Required by [`ThemeToken`].
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellStyle(pub Style);

/// Border styles for replicated leaves: `pane` when a node names no colour of its own,
/// `focused` for the focused pane.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct BorderStyles {
    pub pane: Style,
    pub focused: Style,
}

impl BorderStyles {
    fn of(theme: &Theme) -> Self {
        Self {
            pane: resolve(theme, ThemeToken::PANE_BORDER),
            focused: resolve(theme, ThemeToken::FOCUS_RING),
        }
    }
}

impl Default for BorderStyles {
    fn default() -> Self {
        Self::of(&Theme::default())
    }
}

/// The viewer's `fux.toml#theme` handle.
#[derive(Resource, Debug, Clone)]
pub struct ThemeHandle(pub Handle<Theme>);

/// The style a token stands for: bar fills, bar text, reversed accents (the active tab and
/// notices are drawn in their colour with bar-background text; `default` reverses the
/// terminal's own colours), bold focus ring.
pub fn resolve(theme: &Theme, token: ThemeToken) -> Style {
    let color = theme.color(token);
    match token {
        ThemeToken::BAR_BACKGROUND => Style {
            fg: WireColor::Default,
            bg: color,
            attrs: 0,
        },
        ThemeToken::TAB_ACTIVE | ThemeToken::NOTICE => {
            if color == WireColor::Default {
                Style {
                    fg: WireColor::Default,
                    bg: WireColor::Default,
                    attrs: Style::BOLD | Style::INVERSE,
                }
            } else {
                Style {
                    fg: theme.color(ThemeToken::BAR_BACKGROUND),
                    bg: color,
                    attrs: Style::BOLD,
                }
            }
        }
        ThemeToken::FOCUS_RING => Style {
            fg: color,
            bg: WireColor::Default,
            attrs: Style::BOLD,
        },
        _ => Style {
            fg: color,
            bg: WireColor::Default,
            attrs: 0,
        },
    }
}

/// The resolve pass: every tokened entity on a theme change, only new ones otherwise.
fn apply_theme(
    mut events: MessageReader<AssetEvent<Theme>>,
    handle: Res<ThemeHandle>,
    themes: Res<Assets<Theme>>,
    mut nodes: ParamSet<(
        Query<(&ThemeToken, &mut CellStyle)>,
        Query<(&ThemeToken, &mut CellStyle), Changed<ThemeToken>>,
    )>,
    mut borders: ResMut<BorderStyles>,
) {
    let theme_changed = events.read().any(|event| {
        matches!(event, AssetEvent::Added { id } | AssetEvent::Modified { id } if *id == handle.0.id())
    });
    if !theme_changed && nodes.p1().is_empty() {
        return;
    }
    let default;
    let theme = match themes.get(&handle.0) {
        Some(theme) => theme,
        None => {
            default = Theme::default();
            &default
        }
    };
    if theme_changed {
        borders.set_if_neq(BorderStyles::of(theme));
        for (token, mut style) in &mut nodes.p0() {
            style.set_if_neq(CellStyle(resolve(theme, *token)));
        }
    } else {
        for (token, mut style) in &mut nodes.p1() {
            style.set_if_neq(CellStyle(resolve(theme, *token)));
        }
    }
}

pub struct ThemePlugin;

impl Plugin for ThemePlugin {
    fn build(&self, app: &mut App) {
        let handle = app.world().resource::<AssetServer>().load(THEME_PATH);
        app.insert_resource(ThemeHandle(handle))
            .init_resource::<BorderStyles>()
            .add_systems(
                PostUpdate,
                apply_theme
                    .after(AssetEventSystems)
                    .before(super::paint::paint),
            );
    }
}
