//! Deterministic ECS tests: injected events and time, no sockets, no processes, no sleeps.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::indexing_slicing
)]

use fux::config::Config;
use fux::daemon::{ManagerReply, ManagerRequest};
use fux::ecs::{Effect, Inbound, ManagerOutcome, Session, ViewerRequest};
use fux::ids::{PaneId, TabId, ViewerId};
use fux::layout::Axis;
use fux::proto::attach::{MouseEvent, ServerMessage};
use fux::proto::control::{
    CommandResult, ErrorCode, Event, FocusTarget, Reply, Request, TabAction, WorkspaceAction,
};
use fux::view::{Frame, PaneView};
use std::collections::BTreeMap;

mod harness;
use harness::*;

mod capture;
mod creation;
mod final_records;
mod input;
mod layout;
mod randomized;
mod transfer;
mod workspace;
