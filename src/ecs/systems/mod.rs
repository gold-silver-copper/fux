//! The ordered phases of one step. Every system is exclusive: one logical writer, explicit
//! ordering, mutations visible immediately to the next phase.

pub mod creation;
pub mod final_records;
pub mod input;
pub mod layout;
pub mod lifecycle;
pub mod output;
pub mod requests;
pub mod snapshot;
