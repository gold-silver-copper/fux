//! A specification model of viewer ownership, independent of controller modes,
//! history containers, parser internals and cleanup helpers. Events describe user
//! intent or external changes; only observable controller results are compared.
use super::*;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Event {
    viewer: usize,
    pane: u32,
    op: Op,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
enum Op {
    Wheel,
    Focus,
    Copy,
    Rename,
    SubmitName,
    Escape,
    Resume,
    Buffer,
    Resize,
    Remove,
    Replace,
    Lookup,
    LateReply,
    PasteStart,
    PasteChunk,
    PasteEnd,
    SelectPress,
    Motion,
    Release,
    PrefixText,
    PrefixPaste,
    PrefixCommand,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    Normal,
    Copy(u32),
    Rename(u32),
    Lookup,
}
struct Spec {
    owner: Owner,
    histories: BTreeMap<u32, usize>,
    focus: u32,
    paste: bool,
    drag: Option<u32>,
    tail: bool,
}
impl Default for Spec {
    fn default() -> Self {
        Self {
            owner: Owner::Normal,
            histories: BTreeMap::new(),
            focus: 1,
            paste: false,
            drag: None,
            tail: false,
        }
    }
}
struct Viewer {
    controller: Controller,
    prefix: super::super::input::PrefixFilter,
    frame: Frame,
    spec: Spec,
    lookups: Vec<(u64, bool)>,
    reads: Vec<(u64, PaneId, bool)>,
}

fn run(events: &[Event]) -> Result<()> {
    let mut viewers: Vec<_> = (1..=2)
        .map(|viewer| {
            let mut frame = super::tests::split_frame();
            frame.viewer = crate::ids::ViewerId(viewer);
            frame.server_instance = "trace".into();
            frame.workspace_stream = 1;
            let mut controller = Controller::new(true);
            controller.reconcile(&frame);
            Viewer {
                controller,
                prefix: super::super::input::PrefixFilter::new(
                    crate::commands::ClientBindings::new(1, [(b'x', Action::RenamePane)]),
                ),
                frame,
                spec: Spec::default(),
                lookups: Vec::new(),
                reads: Vec::new(),
            }
        })
        .collect();
    for (tick, event) in events.iter().enumerate() {
        ensure!(
            event.viewer < 2 && (1..=2).contains(&event.pane),
            "invalid trace event"
        );
        let pane = PaneId(event.pane);
        if matches!(event.op, Op::Buffer | Op::Resize | Op::Remove | Op::Replace) {
            for viewer in &mut viewers {
                match event.op {
                    Op::Buffer => {
                        if let Some(view) = viewer.frame.panes.get_mut(&pane) {
                            view.modes.alternate_screen = !view.modes.alternate_screen;
                            viewer.spec.histories.remove(&event.pane);
                            if viewer.spec.owner == Owner::Copy(event.pane) {
                                viewer.spec.owner = Owner::Normal;
                            }
                        }
                    }
                    Op::Resize => {
                        for entry in &mut viewer.frame.layout {
                            entry.rect.height = if entry.rect.height == 6 { 5 } else { 6 };
                        }
                    }
                    Op::Remove => {
                        viewer.frame.panes.remove(&pane);
                        viewer.frame.layout.retain(|entry| entry.pane != pane);
                        viewer.spec.histories.remove(&event.pane);
                        if matches!(viewer.spec.owner, Owner::Copy(id) | Owner::Rename(id) if id == event.pane)
                        {
                            viewer.spec.owner = Owner::Normal;
                        }
                    }
                    Op::Replace => {
                        let stream = viewer.frame.workspace_stream + 1;
                        let viewer_id = viewer.frame.viewer;
                        viewer.frame = super::tests::split_frame();
                        viewer.frame.server_instance = "trace".into();
                        viewer.frame.workspace_stream = stream;
                        viewer.frame.viewer = viewer_id;
                        viewer.spec.histories.clear();
                        viewer.spec.owner = Owner::Normal;
                        viewer.spec.focus = 1;
                    }
                    _ => {}
                }
                if viewer.spec.drag.is_some_and(|drag| {
                    matches!(event.op, Op::Resize | Op::Replace) || drag == event.pane
                }) {
                    viewer.spec.drag = None;
                    viewer.spec.tail = true;
                }
                viewer.controller.reconcile(&viewer.frame);
            }
        } else {
            let viewer = viewers.get_mut(event.viewer).context("viewer")?;
            let exists = viewer.frame.pane(pane).is_some();
            match event.op {
                Op::Focus if exists => {
                    viewer.spec.focus = event.pane;
                    viewer.frame.focused = Some(pane);
                    viewer.controller.reconcile(&viewer.frame);
                }
                Op::Wheel if exists && viewer.spec.owner == Owner::Normal && !viewer.spec.paste => {
                    let disposition = viewer.controller.mouse(
                        MouseEvent {
                            code: 68,
                            column: if event.pane == 1 { 3 } else { 16 },
                            row: 3,
                            release: false,
                        },
                        &viewer.frame,
                    );
                    ensure!(
                        matches!(disposition, MouseDisposition::Local),
                        "wheel not locally owned"
                    );
                    viewer.spec.histories.insert(event.pane, tick);
                }
                Op::Copy | Op::Rename | Op::Lookup
                    if viewer.spec.owner == Owner::Normal && !viewer.spec.paste =>
                {
                    let focus = viewer.spec.focus;
                    if viewer.frame.pane(PaneId(focus)).is_some() {
                        let (action, owner) = match event.op {
                            Op::Copy => (Action::CopyMode, Owner::Copy(focus)),
                            Op::Rename => (Action::RenamePane, Owner::Rename(focus)),
                            _ => (Action::ChooseWorkspace, Owner::Lookup),
                        };
                        ensure!(
                            viewer.controller.enter(action, &viewer.frame),
                            "valid mode entry rejected"
                        );
                        viewer.spec.owner = owner;
                        if matches!(owner, Owner::Copy(_)) {
                            viewer.spec.histories.remove(&focus);
                        }
                        if owner == Owner::Lookup {
                            viewer
                                .lookups
                                .push((viewer.controller.interaction_epoch(), true));
                        }
                    }
                }
                Op::Escape if !viewer.spec.paste => {
                    if viewer.spec.owner != Owner::Normal {
                        ensure!(
                            viewer.controller.feed(27, &viewer.frame).is_none(),
                            "Escape emitted mutation"
                        );
                        viewer.controller.resolve_escape();
                        viewer.spec.owner = Owner::Normal;
                        if viewer.spec.drag.take().is_some() {
                            viewer.spec.tail = true;
                        }
                    } else {
                        viewer.controller.dismiss_history();
                        if let Some(pane) = viewer
                            .spec
                            .histories
                            .iter()
                            .max_by_key(|(_, tick)| *tick)
                            .map(|(pane, _)| *pane)
                        {
                            viewer.spec.histories.remove(&pane);
                        }
                    }
                }
                Op::Resume if viewer.spec.owner == Owner::Normal && !viewer.spec.paste => {
                    viewer.controller.resume_input(&viewer.frame);
                    viewer.spec.histories.remove(&viewer.spec.focus);
                }
                Op::SubmitName if !viewer.spec.paste => {
                    if let Owner::Rename(target) = viewer.spec.owner {
                        let requests: Vec<_> = b"\x15trace\r"
                            .iter()
                            .filter_map(|byte| viewer.controller.feed(*byte, &viewer.frame))
                            .collect();
                        ensure!(
                            matches!(requests.as_slice(), [Request::RenamePane { pane, name, .. }] if pane.0 == target && name == "trace"),
                            "rename duplicated or misdirected"
                        );
                        viewer.spec.owner = Owner::Normal;
                    }
                }
                Op::PasteStart
                    if !viewer.spec.paste
                        && matches!(viewer.spec.owner, Owner::Copy(_) | Owner::Rename(_)) =>
                {
                    for byte in b"\x1b[200~" {
                        ensure!(
                            viewer.controller.feed(*byte, &viewer.frame).is_none(),
                            "paste start mutated target"
                        );
                    }
                    viewer.spec.paste = true;
                }
                Op::PasteChunk | Op::PasteEnd if viewer.spec.paste => {
                    let bytes: &[u8] = if matches!(event.op, Op::PasteEnd) {
                        b"\x1b[201~"
                    } else {
                        b"literal\x01q\r\x1b[31m"
                    };
                    for byte in bytes {
                        ensure!(
                            viewer.controller.feed(*byte, &viewer.frame).is_none(),
                            "paste escaped its owner"
                        );
                    }
                    if matches!(event.op, Op::PasteEnd) {
                        viewer.spec.paste = false;
                    }
                }
                Op::SelectPress
                    if exists
                        && !viewer.spec.paste
                        && matches!(viewer.spec.owner, Owner::Normal | Owner::Copy(_)) =>
                {
                    let disposition = viewer.controller.mouse(
                        MouseEvent {
                            code: 4,
                            column: if event.pane == 1 { 3 } else { 16 },
                            row: 2,
                            release: false,
                        },
                        &viewer.frame,
                    );
                    ensure!(
                        matches!(disposition, MouseDisposition::Local),
                        "selection press not locally owned"
                    );
                    viewer.spec.owner = Owner::Copy(event.pane);
                    viewer.spec.histories.remove(&event.pane);
                    viewer.spec.drag = Some(event.pane);
                    viewer.spec.tail = false;
                }
                Op::Motion | Op::Release
                    if !viewer.spec.paste && (viewer.spec.drag.is_some() || viewer.spec.tail) =>
                {
                    let disposition = viewer.controller.mouse(
                        MouseEvent {
                            code: if matches!(event.op, Op::Motion) {
                                36
                            } else {
                                4
                            },
                            column: if event.pane == 1 { 4 } else { 17 },
                            row: 3,
                            release: matches!(event.op, Op::Release),
                        },
                        &viewer.frame,
                    );
                    ensure!(
                        if viewer.spec.tail {
                            matches!(disposition, MouseDisposition::Ignore)
                        } else {
                            matches!(disposition, MouseDisposition::Local)
                        },
                        "gesture tail reached another owner"
                    );
                    if matches!(event.op, Op::Release) {
                        viewer.spec.drag = None;
                        viewer.spec.tail = false;
                    }
                }
                Op::PrefixText | Op::PrefixPaste | Op::PrefixCommand
                    if viewer.spec.owner == Owner::Normal
                        && !viewer.spec.paste
                        && viewer.frame.pane(PaneId(viewer.spec.focus)).is_some() =>
                {
                    use super::super::input::InputEvent;
                    let bytes: &[u8] = match event.op {
                        Op::PrefixText => b"unicode-\xe2\x98\x83\x1bOA",
                        Op::PrefixPaste => b"\x1b[200~literal\x01x\r\x1b[31m\x1b[201~",
                        _ => b"\x01x",
                    };
                    let mut forwarded = Vec::new();
                    let mut commands = Vec::new();
                    for chunk in bytes.chunks(event.pane as usize) {
                        for output in viewer.prefix.feed(chunk) {
                            match output {
                                InputEvent::Bytes(bytes) => forwarded.extend(bytes),
                                InputEvent::Command(action) => commands.push(action),
                                _ => anyhow::bail!("prefix generated unexpected event"),
                            }
                        }
                    }
                    if matches!(event.op, Op::PrefixCommand) {
                        ensure!(
                            forwarded.is_empty() && commands == [Action::RenamePane],
                            "prefix duplicated or leaked command bytes"
                        );
                        ensure!(
                            viewer.controller.enter(Action::RenamePane, &viewer.frame),
                            "prefix rename rejected"
                        );
                        viewer.spec.owner = Owner::Rename(viewer.spec.focus);
                    } else {
                        ensure!(
                            forwarded == bytes && commands.is_empty(),
                            "application bytes changed or paste became command"
                        );
                        viewer.controller.resume_input(&viewer.frame);
                        viewer.spec.histories.remove(&viewer.spec.focus);
                    }
                }
                Op::LateReply => {
                    for (epoch, valid) in &viewer.lookups {
                        if !valid {
                            ensure!(
                                !viewer.controller.workspaces_loaded_for(
                                    *epoch,
                                    Err(anyhow::anyhow!("stale trace response")),
                                    "default"
                                ),
                                "canceled lookup resurrected"
                            );
                        }
                    }
                    for (request, pane, valid) in &viewer.reads {
                        if !valid {
                            viewer.controller.install_view(ViewReply {
                                request: *request,
                                pane: *pane,
                                view: None,
                                history: 0,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        for viewer in &mut viewers {
            ensure!(
                viewer.controller.owns_input()
                    == (viewer.spec.owner != Owner::Normal || viewer.spec.paste),
                "interaction owner mismatch"
            );
            let mut expected: BTreeSet<_> = viewer.spec.histories.keys().copied().collect();
            if let Owner::Copy(pane) = viewer.spec.owner {
                expected.insert(pane);
            }
            // Invalidation comes from the specification's ownership/lifetime,
            // never from the implementation's pending-request or epoch checks.
            for (_, pane, valid) in &mut viewer.reads {
                if !expected.contains(&pane.0)
                    || matches!(event.op, Op::Replace)
                    || matches!(event.op, Op::Buffer | Op::Remove) && pane.0 == event.pane
                {
                    *valid = false;
                }
            }
            if viewer.spec.owner != Owner::Lookup || matches!(event.op, Op::Replace) {
                for (_, valid) in &mut viewer.lookups {
                    *valid = false;
                }
            }
            let actual: Vec<_> = viewer
                .controller
                .local_views()
                .iter()
                .map(|view| view.pane.0)
                .collect();
            ensure!(
                actual.len() == expected.len()
                    && actual.into_iter().collect::<BTreeSet<_>>() == expected,
                "history ownership mismatch"
            );
            ensure!(
                viewer.controller.take_action().is_none(),
                "unexpected deferred action"
            );
            ensure!(
                viewer.controller.take_manager_request().is_none(),
                "unexpected manager mutation"
            );
            ensure!(
                viewer.controller.take_copied().is_none(),
                "unexpected clipboard mutation"
            );
            if let Some((request, pane, _)) = viewer.controller.take_read() {
                viewer.reads.push((request, pane, true));
            }
            ensure!(
                viewer.controller.local_views().len() <= 2,
                "retained state exceeds pane bound"
            );
        }
    }
    Ok(())
}

fn generated(mut seed: u64, count: usize) -> Vec<Event> {
    let ops = [
        Op::Wheel,
        Op::Focus,
        Op::Copy,
        Op::Rename,
        Op::SubmitName,
        Op::Escape,
        Op::Resume,
        Op::Buffer,
        Op::Resize,
        Op::Remove,
        Op::Replace,
        Op::Lookup,
        Op::LateReply,
        Op::PasteStart,
        Op::PasteChunk,
        Op::PasteEnd,
        Op::SelectPress,
        Op::Motion,
        Op::Release,
        Op::PrefixText,
        Op::PrefixPaste,
        Op::PrefixCommand,
    ];
    (0..count)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            Event {
                viewer: (seed & 1) as usize,
                pane: ((seed >> 1) & 1) as u32 + 1,
                op: ops
                    .get((seed >> 2) as usize % ops.len())
                    .copied()
                    .unwrap_or(Op::Escape),
            }
        })
        .collect()
}

/// Deletion minimization retains the same failure, rather than accepting a new
/// unrelated failure caused by removing an event's prerequisites.
fn minimize(mut events: Vec<Event>, failure: &str) -> Vec<Event> {
    let mut width = events.len() / 2;
    while width > 0 {
        let mut start = 0;
        while start + width <= events.len() {
            let mut candidate = events.clone();
            candidate.drain(start..start + width);
            if run(&candidate).is_err_and(|error| error.to_string() == failure) {
                events = candidate;
            } else {
                start += 1;
            }
        }
        width /= 2;
    }
    events
}

#[test]
fn generated_controller_traces() -> Result<()> {
    if let Some(path) = std::env::var_os("FUX_CONTROL_TRACE") {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(65537)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 65536, "trace exceeds byte bound");
        let events: Vec<Event> = serde_json::from_slice(&bytes)?;
        ensure!(events.len() <= 256, "trace exceeds event bound");
        return run(&events);
    }
    let seeds: Vec<u64> = match std::env::var("FUX_CONTROL_SEED") {
        Ok(seed) => vec![seed.parse()?],
        Err(_) => (1..=256).collect(),
    };
    for seed in seeds {
        let events = generated(seed, 128);
        if let Err(error) = run(&events) {
            let failure = error.to_string();
            let minimized = minimize(events, &failure);
            let directory = std::env::temp_dir().join(format!(
                "fux-control-trace-{}-{}",
                std::process::id(),
                seed
            ));
            std::fs::create_dir(&directory)?;
            let path = directory.join("trace.json");
            std::fs::write(&path, serde_json::to_vec_pretty(&minimized)?)?;
            std::fs::write(
                directory.join("failure.json"),
                serde_json::to_vec_pretty(&serde_json::json!({
                    "seed": seed, "failure": failure, "generated_events": 128, "minimized_events": minimized.len()
                }))?,
            )?;
            anyhow::bail!(
                "{failure}; seed {seed}; replay: FUX_CONTROL_TRACE={} cargo test -p fux --lib generated_controller_traces",
                path.display()
            );
        }
    }
    Ok(())
}
