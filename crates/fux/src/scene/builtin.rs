//! Built-in layouts (prompt 3.7: `bsn!` provides the default ones), selected by name through
//! `scene.restore`. Each is one root `main` whose leaves run the configured default command
//! ([`shell`]); a user file of the same name shadows the built-in.

use bevy_ecs::prelude::*;
use bevy_scene::prelude::*;
use bevy_ui::{FlexDirection, Node};

use super::templates::{pane, root, shell};

/// Names in the order `scene.list` reports them.
pub const NAMES: &[&str] = &["two_column", "two_row", "three_column", "main_side"];

/// Two panes side by side, equal width.
pub fn two_column() -> impl Scene {
    bsn! {
        root("main")
        Node { flex_direction: FlexDirection::Row }
        Children [
            (#left pane(shell())),
            (#right pane(shell())),
        ]
    }
}

/// Two panes stacked, equal height.
pub fn two_row() -> impl Scene {
    bsn! {
        root("main")
        Node { flex_direction: FlexDirection::Column }
        Children [
            (#top pane(shell())),
            (#bottom pane(shell())),
        ]
    }
}

/// Three panes side by side, equal width.
pub fn three_column() -> impl Scene {
    bsn! {
        root("main")
        Node { flex_direction: FlexDirection::Row }
        Children [
            (#left pane(shell())),
            (#middle pane(shell())),
            (#right pane(shell())),
        ]
    }
}

/// A main pane taking two thirds and a side pane taking one third.
pub fn main_side() -> impl Scene {
    bsn! {
        root("main")
        Node { flex_direction: FlexDirection::Row }
        Children [
            (#main pane(shell()) Node { flex_grow: 2.0 }),
            (#side pane(shell()) Node { flex_grow: 1.0 }),
        ]
    }
}

/// The built-in layout of that name.
pub fn scene(name: &str) -> Option<Box<dyn Scene>> {
    Some(match name {
        "two_column" => Box::new(two_column()),
        "two_row" => Box::new(two_row()),
        "three_column" => Box::new(three_column()),
        "main_side" => Box::new(main_side()),
        _ => return None,
    })
}
