//! The machine catalog: `$XDG_CONFIG_HOME/zor/machines.json` (multi-machine-supervision.md:50-56),
//! version 1, a `machines` array of stable `id`, editable `name`, an optional `control` transport
//! and `attachments` keyed by fux workspace. The file and its directory must be private to the
//! user; limits are 32 machines, 64 attachment bindings per machine and 256 KiB. Reads never
//! follow symlinks; writes are atomic (0600 temp + fsync + rename).

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::transport::Transport;
use crate::model::valid_id;

pub const VERSION: u32 = 1;
pub const MAX_MACHINES: usize = 32;
pub const MAX_BINDINGS: usize = 64;
pub const MAX_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub version: u32,
    #[serde(default)]
    pub machines: Vec<MachineEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineEntry {
    /// Stable across renames (multi-machine-supervision.md:50).
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub control: Option<Transport>,
    /// Attachment bindings keyed by the remote fux workspace; independent of `control`
    /// (multi-machine-supervision.md:62-64).
    #[serde(default)]
    pub attachments: BTreeMap<String, Transport>,
}

#[derive(Debug)]
pub enum CatalogError {
    Io(std::io::Error),
    /// The file or its directory is not private to this user, or is not a regular file.
    Insecure(String),
    TooLarge(u64),
    Json(serde_json::Error),
    Invalid(String),
    /// No catalog path is configured (a headless server).
    Unconfigured,
}

impl core::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "machine catalog: {e}"),
            Self::Insecure(what) => write!(f, "machine catalog: {what} is not private to this user"),
            Self::TooLarge(n) => write!(f, "machine catalog: {n} bytes exceeds {MAX_BYTES}"),
            Self::Json(e) => write!(f, "machine catalog: {e}"),
            Self::Invalid(why) => write!(f, "machine catalog: {why}"),
            Self::Unconfigured => f.write_str("machine catalog: no catalog path is configured"),
        }
    }
}

impl core::error::Error for CatalogError {}

impl From<std::io::Error> for CatalogError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for CatalogError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

fn private(path: &Path, meta: &fs::Metadata) -> Result<(), CatalogError> {
    let uid = nix::unistd::getuid().as_raw();
    if meta.uid() != uid || meta.permissions().mode() & 0o077 != 0 {
        return Err(CatalogError::Insecure(path.display().to_string()));
    }
    Ok(())
}

/// Reads a private regular file in a private directory; `None` when it does not exist.
pub fn read_private(path: &Path) -> Result<Option<Vec<u8>>, CatalogError> {
    if path.as_os_str().is_empty() {
        return Err(CatalogError::Unconfigured);
    }
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !meta.is_file() {
        return Err(CatalogError::Insecure(path.display().to_string()));
    }
    private(path, &meta)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        private(parent, &fs::symlink_metadata(parent)?)?;
    }
    if meta.len() > MAX_BYTES {
        return Err(CatalogError::TooLarge(meta.len()));
    }
    let file = OpenOptions::new().read(true).custom_flags(nix::libc::O_NOFOLLOW).open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() { return Err(CatalogError::Insecure(path.display().to_string())); }
    private(path, &opened)?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES { return Err(CatalogError::TooLarge(bytes.len() as u64)); }
    Ok(Some(bytes))
}

/// Atomic private write (0600 temp + fsync + rename + directory sync); the directory is
/// created 0700 when missing and refused when not private.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), CatalogError> {
    if path.as_os_str().is_empty() {
        return Err(CatalogError::Unconfigured);
    }
    if bytes.len() as u64 > MAX_BYTES {
        return Err(CatalogError::TooLarge(bytes.len() as u64));
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        fux::paths::private_dir(parent)
            .map_err(|e| CatalogError::Insecure(format!("{}: {e}", parent.display())))?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let written = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        fs::rename(&temporary, path)?;
        if let Some(parent) = parent {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok::<(), std::io::Error>(())
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&temporary);
        return Err(e.into());
    }
    Ok(())
}

impl Catalog {
    pub fn empty() -> Self {
        Self {
            version: VERSION,
            machines: Vec::new(),
        }
    }

    /// Reads and validates the catalog; a missing file is an empty catalog, but an existing
    /// file must be a private regular file in a private directory.
    pub fn load(path: &Path) -> Result<Self, CatalogError> {
        match read_private(path)? {
            Some(bytes) => Self::parse(&bytes),
            None => Ok(Self::empty()),
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, CatalogError> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(CatalogError::TooLarge(bytes.len() as u64));
        }
        let catalog: Self = serde_json::from_slice(bytes)?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn validate(&self) -> Result<(), CatalogError> {
        let invalid = |why: String| Err(CatalogError::Invalid(why));
        if self.version != VERSION {
            return invalid(format!("version {} is not {VERSION}", self.version));
        }
        if self.machines.len() > MAX_MACHINES {
            return invalid(format!("{} machines exceed {MAX_MACHINES}", self.machines.len()));
        }
        for (i, machine) in self.machines.iter().enumerate() {
            if machine.id.eq_ignore_ascii_case("local") || machine.name.eq_ignore_ascii_case("local") {
                return invalid("local is reserved for this controller".into());
            }
            if !valid_id(&machine.id) {
                return invalid(format!("machine {i}: invalid id {:?}", machine.id));
            }
            if !valid_id(&machine.name) {
                return invalid(format!("machine {:?}: invalid name {:?}", machine.id, machine.name));
            }
            if machine.attachments.len() > MAX_BINDINGS {
                return invalid(format!(
                    "machine {:?}: {} attachment bindings exceed {MAX_BINDINGS}",
                    machine.id,
                    machine.attachments.len()
                ));
            }
            for workspace in machine.attachments.keys() {
                if !valid_id(workspace) {
                    return invalid(format!(
                        "machine {:?}: invalid workspace {workspace:?}",
                        machine.id
                    ));
                }
            }
            if let Some(transport) = &machine.control {
                transport.validate().map_err(|e| {
                    CatalogError::Invalid(format!("machine {:?}: control: {e}", machine.id))
                })?;
            }
            for (workspace, transport) in &machine.attachments {
                transport.validate().map_err(|e| {
                    CatalogError::Invalid(format!(
                        "machine {:?}: attachment {workspace:?}: {e}",
                        machine.id
                    ))
                })?;
            }
            if self.machines[..i]
                .iter()
                .any(|m| m.id == machine.id || m.name == machine.name || m.id == machine.name || m.name == machine.id)
            {
                return invalid(format!(
                    "machine {:?} ({:?}) duplicates an earlier id or name",
                    machine.id, machine.name
                ));
            }
        }
        Ok(())
    }

    /// Atomic private write; the directory is created 0700 when missing.
    pub fn save(&self, path: &Path) -> Result<(), CatalogError> {
        self.validate()?;
        write_private(path, &serde_json::to_vec_pretty(self)?)
    }

    /// By stable id first, then by name; unknown selectors never fall back to anything.
    pub fn find(&self, selector: &str) -> Option<&MachineEntry> {
        self.machines
            .iter()
            .find(|m| m.id == selector)
            .or_else(|| self.machines.iter().find(|m| m.name == selector))
    }

    pub fn find_mut(&mut self, selector: &str) -> Option<&mut MachineEntry> {
        let index = self
            .machines
            .iter()
            .position(|m| m.id == selector)
            .or_else(|| self.machines.iter().position(|m| m.name == selector))?;
        self.machines.get_mut(index)
    }

    /// Adds a machine with a fresh stable id derived from its name; refuses a name in use.
    pub fn add(&mut self, entry: MachineEntry) -> Result<(), CatalogError> {
        if self.machines.len() >= MAX_MACHINES {
            return Err(CatalogError::Invalid(format!(
                "{MAX_MACHINES} machines are the limit"
            )));
        }
        if self.machines.iter().any(|m| m.name == entry.name || m.id == entry.id || m.name == entry.id || m.id == entry.name) {
            return Err(CatalogError::Invalid(format!(
                "machine {:?} already exists",
                entry.name
            )));
        }
        self.machines.push(entry);
        if let Err(error) = self.validate() {
            self.machines.pop();
            return Err(error);
        }
        Ok(())
    }
}

/// A fresh stable id: `m-<12 random hex>`.
pub fn fresh_id() -> Result<String, CatalogError> {
    let mut hex = fux::attach::random_hex256().map_err(|e| CatalogError::Invalid(e.to_string()))?;
    hex.truncate(12);
    Ok(format!("m-{hex}"))
}
