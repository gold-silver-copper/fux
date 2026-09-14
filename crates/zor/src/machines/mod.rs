//! Zor-owned supervision targets. Profiles confer no transport or application authority.
mod catalog;
pub mod connection;
pub mod intents;
pub mod supervision;

pub use catalog::{Binding, Catalog, Machine};
