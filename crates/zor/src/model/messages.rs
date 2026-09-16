//! The World never holds an OS handle: the runner writes a bounded batch of [`Inbound`]
//! messages before each `update` and drains [`Effect`]s after it. Both are Bevy `Messages`
//! used only within one update.

use bevy_ecs::prelude::*;
use serde_json::Value;

pub use fux::model::Signal;

/// Runner → World.
#[derive(Message, Debug)]
pub enum Inbound {
    /// A provider subprocess (Codex app-server, OpenCode plugin, ...) started for an attempt.
    ProviderStarted {
        attempt: Entity,
        pid: u32,
    },
    ProviderOutput {
        attempt: Entity,
        bytes: Vec<u8>,
    },
    ProviderExited {
        attempt: Entity,
        code: i32,
    },
    /// A check leader exited (`code`, `None` on signal), timed out or could not run
    /// (`problem`: spawn/output/timeout cause → Uncertain); streams already clipped to
    /// `MAX_FINAL_OUTPUT_BYTES` each (CHECKS.md:27-28, 46-49).
    CheckDone {
        check: Entity,
        code: Option<i32>,
        stdout: String,
        stderr: String,
        truncated: bool,
        problem: Option<String>,
    },
    /// A git command requested by `Effect::RunGit { op }` finished.
    GitDone {
        op: u64,
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    /// One item of `fux/events+watch`, with the server-wide cursor.
    FuxEvent {
        cursor: u64,
        name: String,
        body: Value,
    },
    /// The events stream could not resume losslessly: entries `since..resume` were lost.
    FuxGap {
        since: u64,
        resume: u64,
    },
    /// The events stream connected (`Some(instance)`) or dropped (`None`).
    FuxLink {
        instance: Option<String>,
    },
    /// Reply to `Effect::FuxCall { call }`.
    FuxReply {
        call: u64,
        result: Result<Value, String>,
    },
    /// A plugin process (action, build, startup or hook) exited.
    PluginExited {
        plugin: Entity,
        run: u64,
        code: Option<i32>,
    },
    Signal(Signal),
    /// An adapter queued work for an in-World drain (a parked BRP request); carries nothing.
    Wake,
}

/// World → runner → adapters.
#[derive(Message, Debug)]
pub enum Effect {
    SpawnProvider {
        attempt: Entity,
        argv: Vec<String>,
        cwd: Option<String>,
        env: Vec<(String, String)>,
    },
    WriteProvider {
        attempt: Entity,
        bytes: Vec<u8>,
    },
    RunCheck {
        check: Entity,
        argv: Vec<String>,
        cwd: String,
        timeout_ms: u64,
    },
    /// Kill the process group of a running check (`zor/check.cancel`); answered by `CheckDone`.
    KillCheck {
        check: Entity,
    },
    RunGit {
        op: u64,
        argv: Vec<String>,
        cwd: String,
    },
    /// One typed `fux/*` call; answered by `Inbound::FuxReply { call }`.
    FuxCall {
        call: u64,
        method: String,
        params: Value,
    },
    /// Run a plugin process with its environment; answered by `Inbound::PluginExited`.
    RunPlugin {
        plugin: Entity,
        run: u64,
        argv: Vec<String>,
        cwd: Option<String>,
        env: Vec<(String, String)>,
        log: std::path::PathBuf,
    },
    /// Kill a plugin process group (disable, restart); answered by `PluginExited`.
    KillPlugin {
        plugin: Entity,
        run: u64,
    },
    /// The World has nothing live left; the runner returns `AppExit`.
    Exit {
        code: u8,
    },
}
