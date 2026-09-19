//! Asset-style asynchronous preparation: worker owns filesystem I/O, scheduled completion
//! alone mutates the World. Loading validates data; it never authorizes command execution.
//! No watcher is added: linked manifests reload at explicit link and server startup.
use super::{HookCursors, Manifest, PluginError, PluginPaths, Res};
use crate::model::Inbound;
use async_channel::{Receiver, Sender};
use bevy_ecs::prelude::*;
use bevy_tasks::IoTaskPool;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_OPERATIONS: usize = 64;
const MAX_PENDING: usize = 8;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    pub operation: u64,
    pub state: String,
    pub name: Option<String>,
    pub problem: Option<String>,
}
struct Prepared {
    manifest: Manifest,
    file: PathBuf,
    cursors: HookCursors,
    retired: Vec<(PathBuf, fux::remote::client::Descriptor)>,
}
struct Completion {
    id: u64,
    reload: Option<Entity>,
    enabled: bool,
    result: Result<Prepared, String>,
}
#[derive(Resource)]
pub(super) struct Loading {
    next: u64,
    records: std::collections::VecDeque<Operation>,
    sender: Sender<Completion>,
    receiver: Receiver<Completion>,
    pub(super) wake: Option<Sender<Inbound>>,
}
impl Default for Loading {
    fn default() -> Self {
        let (sender, receiver) = async_channel::bounded(MAX_PENDING + super::MAX_PLUGINS);
        Self {
            next: 0,
            records: Default::default(),
            sender,
            receiver,
            wake: None,
        }
    }
}

pub(super) fn submit(world: &mut World, path: PathBuf, enabled: bool, copy: bool) -> Res<u64> {
    let paths = world.resource::<PluginPaths>().clone();
    let mut loading = world.resource_mut::<Loading>();
    if loading
        .records
        .iter()
        .filter(|r| r.state == "pending")
        .count()
        >= MAX_PENDING
    {
        return Err(PluginError::Refused("plugin import queue full".into()));
    }
    if loading.records.len() >= MAX_OPERATIONS
        && let Some(index) = loading.records.iter().position(|r| r.state != "pending")
    {
        loading.records.remove(index);
    }
    loading.next += 1;
    let id = loading.next;
    loading.records.push_back(Operation {
        operation: id,
        state: "pending".into(),
        name: None,
        problem: None,
    });
    launch(&loading, paths, path, enabled, copy, id, None);
    Ok(id)
}

pub(super) fn reload(world: &mut World, entity: Entity, path: PathBuf) {
    let paths = world.resource::<PluginPaths>().clone();
    launch(
        world.resource::<Loading>(),
        paths,
        path,
        false,
        false,
        0,
        Some(entity),
    );
}

fn launch(
    loading: &Loading,
    paths: PluginPaths,
    path: PathBuf,
    enabled: bool,
    copy: bool,
    id: u64,
    reload: Option<Entity>,
) {
    let sender = loading.sender.clone();
    let wake = loading.wake.clone();
    IoTaskPool::get()
        .spawn(async move {
            let result = prepare(&paths, &path, copy, reload.is_some());
            if sender
                .send(Completion {
                    id,
                    reload,
                    enabled,
                    result,
                })
                .await
                .is_ok()
                && let Some(wake) = wake
            {
                let _ = wake.try_send(Inbound::Wake);
            }
        })
        .detach();
}

fn prepare(
    paths: &PluginPaths,
    path: &std::path::Path,
    copy: bool,
    reload: bool,
) -> Result<Prepared, String> {
    // Import publication is serialized to prevent two requests replacing one plugin while
    // another copy is in flight. Only bounded workers wait here, never the World.
    static IMPORTS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = IMPORTS.lock().map_err(|_| "plugin import lock poisoned")?;
    let source = super::source_dir(path).map_err(|e| e.to_string())?;
    if !reload && source.starts_with(&paths.root) {
        return Err("cannot import from the host directory".into());
    }
    let manifest = Manifest::read(&source)
        .and_then(|m| m.for_platform(super::manifest::current_platform()))
        .map_err(|e| e.to_string())?;
    for action in &manifest.actions {
        if super::qualified_action(&manifest.name, &action.id).len() > crate::model::ids::MAX_ID_LEN
        {
            return Err("qualified action id exceeds model bound".into());
        }
    }
    std::fs::create_dir_all(paths.state_dir(&manifest.name)).map_err(|e| e.to_string())?;
    let cursors = super::read_cursors(&paths.cursors(&manifest.name))?;
    let file = if copy {
        let target = paths.install_dir(&manifest.name);
        let staging = paths.dir(&manifest.name).join("plugin.new");
        let previous = paths.dir(&manifest.name).join("plugin.previous");
        if staging.exists() {
            std::fs::remove_dir_all(&staging).map_err(|e| e.to_string())?;
        }
        super::copy_tree(&source, &staging, &mut 0)?;
        let copied = Manifest::read(&staging)
            .and_then(|m| m.for_platform(super::manifest::current_platform()))
            .map_err(|e| e.to_string())?;
        if copied != manifest {
            return Err("manifest changed during import; published version left untouched".into());
        }
        if previous.exists() {
            std::fs::remove_dir_all(&previous).map_err(|e| e.to_string())?;
        }
        let existed = target.exists();
        if existed {
            std::fs::rename(&target, &previous).map_err(|e| e.to_string())?;
        }
        if let Err(e) = std::fs::rename(&staging, &target) {
            if existed {
                let _ = std::fs::rename(&previous, &target);
            }
            return Err(e.to_string());
        }
        if existed {
            let _ = std::fs::remove_dir_all(previous);
        }
        target.join(super::manifest::MANIFEST_FILE)
    } else {
        source.join(super::manifest::MANIFEST_FILE)
    };
    let mut retired = Vec::new();
    if reload {
        if let Ok(entries) = std::fs::read_dir(paths.dir(&manifest.name).join("runs")) {
            for entry in entries.take(super::MAX_RUNS_RETAINED * 2 + 1) {
                let entry = entry.map_err(|e| e.to_string())?;
                if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
                    continue;
                }
                let path = entry.path();
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{}-", paths.session))
                {
                    continue;
                }
                if !path.to_string_lossy().ends_with(".fux.brp.json") {
                    continue;
                }
                let descriptor =
                    fux::remote::client::read_descriptor(&path).map_err(|e| e.to_string())?;
                retired.push((path, descriptor));
                if retired.len() > super::MAX_RUNS_RETAINED * 2 {
                    return Err(
                        "too many unreconciled fux grants; explicit cleanup required".into(),
                    );
                }
            }
        }
        if let Ok(entries) = std::fs::read_dir(paths.dir(&manifest.name)) {
            for entry in entries.take(256) {
                let entry = entry.map_err(|e| e.to_string())?;
                if entry.file_type().map_err(|e| e.to_string())?.is_file()
                    && entry
                        .file_name()
                        .to_string_lossy()
                        .ends_with(".zor.brp.json")
                    && fux::remote::client::read_descriptor(&entry.path())
                        .is_ok_and(|d| d.instance != paths.session)
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
    Ok(Prepared {
        manifest,
        file,
        cursors,
        retired,
    })
}

pub(super) fn collect(world: &mut World) {
    while let Ok(completion) = world.resource::<Loading>().receiver.try_recv() {
        if let Some(plugin) = completion.reload {
            if world.get::<crate::model::HostedPlugin>(plugin).is_none() {
                continue;
            }
            if world.get::<super::Loaded>(plugin).is_some() {
                continue;
            }
            match completion.result {
                Ok(prepared) => {
                    let name = super::name_of(world, plugin);
                    if prepared.manifest.name != name {
                        world
                            .entity_mut(plugin)
                            .insert(super::PluginProblem("manifest name changed".into()));
                        continue;
                    }
                    for (path, descriptor) in prepared.retired {
                        super::fux_call(
                            world,
                            super::Pending::Retire { path },
                            "fux/token.revoke",
                            serde_json::json!({"revoke": descriptor.token, "_expected_instance": descriptor.instance}),
                        );
                    }
                    let root = prepared
                        .file
                        .parent()
                        .map(std::path::Path::to_path_buf)
                        .unwrap_or_default();
                    let result = super::apply_manifest(world, plugin, prepared.manifest, root);
                    if let Err(e) = result {
                        world
                            .entity_mut(plugin)
                            .insert(super::PluginProblem(e.to_string()));
                    } else if let Some(mut hook) = world.get_mut::<super::Hook>(plugin) {
                        hook.cursors = prepared.cursors;
                    }
                }
                Err(e) => {
                    world.entity_mut(plugin).insert(super::PluginProblem(e));
                }
            }
            continue;
        }
        let name = completion
            .result
            .as_ref()
            .ok()
            .map(|prepared| prepared.manifest.name.clone());
        let result = completion.result.and_then(|prepared| {
            super::register(
                world,
                prepared.manifest,
                prepared.file,
                completion.enabled,
                prepared.cursors,
            )
            .map_err(|e| format!("import prepared but registration failed: {e}"))
        });
        if let Some(record) = world
            .resource_mut::<Loading>()
            .records
            .iter_mut()
            .find(|r| r.operation == completion.id)
        {
            record.name = name;
            match result {
                Ok(_) => record.state = "complete".into(),
                Err(e) => {
                    record.state = "failed".into();
                    record.problem = Some(e);
                }
            }
        }
    }
}

pub fn operation(world: &World, id: u64) -> Res<Operation> {
    world
        .resource::<Loading>()
        .records
        .iter()
        .find(|r| r.operation == id)
        .cloned()
        .ok_or_else(|| PluginError::NotFound(format!("plugin operation {id}")))
}
