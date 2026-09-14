//! The ordered phases of one step. Every system is exclusive: one logical writer, explicit
//! ordering, mutations visible immediately to the next phase.

pub mod creation;
pub mod layout;
pub mod lifecycle;
pub mod output;
pub mod requests;
mod requests_control;
pub mod snapshot;

pub mod input;

pub mod final_records;

pub mod layout_archive;
pub mod layout_control;

pub mod transfer;
