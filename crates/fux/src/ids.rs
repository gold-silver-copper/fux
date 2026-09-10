//! Stable, session-scoped public identities. These are the identities protocols and viewers
//! use; they are never `bevy_ecs::entity::Entity` handles. Counters increase monotonically for
//! the lifetime of one server process and are never recycled, so a late request naming a
//! closed object fails instead of addressing a replacement.

use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct PaneId(pub u32);

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TabId(pub u32);

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ViewerId(pub u64);

macro_rules! display_inner {
    ($($id:ident),+) => {$(
        impl std::fmt::Display for $id {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    )+};
}
display_inner!(PaneId, TabId, ViewerId);

/// Validates a workspace name that also names socket and descriptor files.
pub fn validate_workspace_name(name: &str) -> Result<(), InvalidName> {
    if name.is_empty()
        || name.len() > 64
        || name == "."
        || name == ".."
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(InvalidName);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("workspace names use 1-64 ASCII letters, digits, `.`, `_` or `-`")]
pub struct InvalidName;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_names_cannot_escape_runtime_directories() {
        for name in ["", ".", "..", "../x", "/abs", "a/b", "a\\b", "nul\0", "ü"] {
            assert!(validate_workspace_name(name).is_err(), "accepted {name:?}");
        }
        for name in ["default", "ws-2", "my.project_1"] {
            assert!(validate_workspace_name(name).is_ok());
        }
    }
}
