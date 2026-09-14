//! The ordered phases of one step. Most systems are typed; four take `&mut World` because they
//! must observe their own mutations within the same batch (see docs/design.md "Systems").

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
