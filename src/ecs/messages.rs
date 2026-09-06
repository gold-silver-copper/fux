//! Typed inbound events, viewer requests and outbound effects. These are the only things that
//! cross the boundary between operating-system adapters and the ECS World.

use crate::ids::{PaneId, ViewerId};
use crate::proto::attach::{MouseEvent, ServerMessage};
use crate::proto::control;
use bevy_ecs::prelude::*;
use std::path::PathBuf;

/// Correlates a control or manager reply with the connection waiting for it.
pub type ReplyToken = u64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewerRequest {
    Input(Vec<u8>),
    Mouse {
        event: MouseEvent,
        generation: u64,
    },
    Control(control::Request),
    View {
        request: u64,
        pane: PaneId,
        offset: u32,
    },
    Resize {
        rows: u16,
        cols: u16,
    },
    Detach,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManagerAction {
    List,
    /// Server identity and limits.
    Info,
    /// `None` applies the documented default rule: create `default` when no workspace exists,
    /// otherwise attach to the most recently attached workspace.
    Resolve {
        name: Option<String>,
    },
    Kill {
        name: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManagerOutcome {
    Names(Vec<String>),
    Attach { name: String, created: bool },
    Info(Box<crate::proto::control::ServerInfo>),
    Failed(String),
}

#[derive(Message, Clone, Debug, PartialEq, Eq)]
pub enum Inbound {
    PaneOutput {
        pane: PaneId,
        bytes: Vec<u8>,
    },
    PaneEof {
        pane: PaneId,
    },
    PaneExited {
        pane: PaneId,
        code: u32,
    },
    SpawnCompleted {
        pane: PaneId,
        result: Result<u32, String>,
    },
    ViewerAttached {
        viewer: ViewerId,
        workspace: String,
        rows: u16,
        cols: u16,
    },
    ViewerGone {
        viewer: ViewerId,
    },
    ViewerRequest {
        viewer: ViewerId,
        request: ViewerRequest,
    },
    ControlRequest {
        workspace: String,
        request: control::Request,
        token: ReplyToken,
    },
    Manager {
        action: ManagerAction,
        token: ReplyToken,
    },
    Shutdown,
}

#[derive(Message, Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    SpawnPane {
        pane: PaneId,
        argv: Vec<String>,
        cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
        rows: u16,
        cols: u16,
    },
    WriteInput {
        pane: PaneId,
        bytes: Vec<u8>,
    },
    ResizePty {
        pane: PaneId,
        rows: u16,
        cols: u16,
    },
    /// SIGHUP now, SIGKILL after `grace_ms`, then reap; the exit arrives as `PaneExited`.
    Terminate {
        pane: PaneId,
        grace_ms: u64,
    },
    /// The pane no longer exists in the World; drop its handles and join its threads.
    ReleasePane {
        pane: PaneId,
    },
    ToViewer {
        viewer: ViewerId,
        message: ServerMessage,
    },
    CloseViewer {
        viewer: ViewerId,
    },
    ControlReply {
        token: ReplyToken,
        reply: control::Reply,
    },
    Manager {
        token: ReplyToken,
        outcome: ManagerOutcome,
    },
    Event {
        workspace: String,
        event: control::Event,
    },
    /// Bind the workspace's sockets and publish its descriptor.
    WorkspaceOpened {
        name: String,
    },
    /// Close the workspace's sockets and remove its descriptor.
    WorkspaceClosed {
        name: String,
    },
    /// No workspace remains; the server may exit.
    Idle,
}

/// Who asked for an operation, so replies and focus follow the right party.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requester {
    Viewer(ViewerId),
    Control(ReplyToken),
    Manager(ReplyToken),
}
