use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::SocketAddr,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

const MAX_MACHINES: usize = 32;
const MAX_BINDINGS: usize = 64;
const MAX_BYTES: u64 = 256 * 1024;

/// One independently authorized koh service. Credentials remain in koh-owned key files.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub endpoint: String,
    pub key_file: PathBuf,
    pub direct: Option<SocketAddr>,
    pub relay_url: Option<String>,
}
impl Binding {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.endpoint.len() == 64 && self.endpoint.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "endpoint must be a koh endpoint ID (64 hexadecimal characters)"
        );
        ensure!(self.key_file.is_absolute(), "key-file must be absolute");
        ensure!(
            self.direct.is_none() || self.relay_url.is_none(),
            "direct and relay-url are mutually exclusive"
        );
        if let Some(url) = &self.relay_url {
            ensure!(
                url.len() <= 2048
                    && (url.starts_with("https://") || url.starts_with("http://"))
                    && !url.contains('@')
                    && !url.chars().any(|ch| ch.is_control() || ch.is_whitespace()),
                "relay-url must be an HTTP(S) URL without credentials or whitespace"
            );
        }
        Ok(())
    }
}

/// A machine's stable ID survives renaming and connection changes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    pub id: String,
    pub name: String,
    pub control: Option<Binding>,
    pub attachments: BTreeMap<String, Binding>,
}
impl Machine {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.id.len() == 32
                && self
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "invalid stable machine ID"
        );
        valid_name(&self.name)?;
        ensure!(
            self.attachments.len() <= MAX_BINDINGS,
            "attachment binding limit exceeded"
        );
        if let Some(binding) = &self.control {
            binding.validate()?;
        }
        for (workspace, binding) in &self.attachments {
            ensure!(
                crate::fux::endpoint::valid_name(workspace),
                "invalid workspace binding name"
            );
            binding.validate()?;
            ensure!(
                self.control.as_ref().is_none_or(|control| !control
                    .endpoint
                    .eq_ignore_ascii_case(&binding.endpoint)),
                "control and attachment require distinct service endpoints"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub version: u32,
    pub machines: Vec<Machine>,
}
impl Default for Catalog {
    fn default() -> Self {
        Self {
            version: 1,
            machines: Vec::new(),
        }
    }
}

fn valid_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 48
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            && !name.eq_ignore_ascii_case("local")
            && !(name.len() == 32 && name.bytes().all(|byte| byte.is_ascii_hexdigit())),
        "machine name must be 1–48 ASCII letters, digits, '.', '_' or '-'; Local and stable IDs are reserved"
    );
    Ok(())
}

impl Catalog {
    pub fn path(explicit: Option<PathBuf>) -> Result<PathBuf> {
        let path = if let Some(path) = explicit {
            path
        } else if let Some(root) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
            PathBuf::from(root).join("zor/machines.json")
        } else {
            PathBuf::from(std::env::var_os("HOME").context("set HOME or XDG_CONFIG_HOME")?)
                .join(".config/zor/machines.json")
        };
        ensure!(
            path.is_absolute() && path.file_name().is_some(),
            "machines-file must be absolute"
        );
        Ok(path)
    }

    pub fn load(path: &Path) -> Result<Self> {
        ensure!(path.is_absolute(), "machines-file must be absolute");
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).context("open machine catalog"),
        };
        let parent = path.parent().context("machine catalog parent")?;
        local_ipc::ensure_private_directory(parent)?;
        private_file(&file)?;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "machine catalog exceeds byte limit"
        );
        let catalog: Self = serde_json::from_slice(&bytes).context("invalid machine catalog")?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn resolve(&self, selector: &str) -> Result<&Machine> {
        ensure!(
            !selector.eq_ignore_ascii_case("local"),
            "Local is not a remote machine profile"
        );
        self.machines
            .iter()
            .find(|machine| machine.id == selector || machine.name.eq_ignore_ascii_case(selector))
            .with_context(|| format!("unknown machine {selector:?}; use `zor machine list`"))
    }

    fn validate(&self) -> Result<()> {
        ensure!(self.version == 1, "unsupported machine catalog version");
        ensure!(
            self.machines.len() <= MAX_MACHINES,
            "machine limit exceeded"
        );
        let mut ids = std::collections::BTreeSet::new();
        let mut names = std::collections::BTreeSet::new();
        for machine in &self.machines {
            machine.validate()?;
            ensure!(ids.insert(&machine.id), "duplicate machine ID");
            ensure!(
                names.insert(machine.name.to_ascii_lowercase()),
                "duplicate machine name"
            );
        }
        Ok(())
    }

    /// Persistent lock plus atomic replacement avoids lost concurrent configuration edits.
    pub fn edit<T>(path: &Path, update: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        ensure!(path.is_absolute(), "machines-file must be absolute");
        let parent = path.parent().context("machine catalog parent")?;
        local_ipc::ensure_private_directory(parent)?;
        let mut lock_name = path
            .file_name()
            .context("machine catalog filename")?
            .to_os_string();
        lock_name.push(".lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(parent.join(lock_name))?;
        private_file(&file)?;
        ensure!(
            file.metadata()?.len() == 0,
            "machine catalog lock must be empty"
        );
        let _lock = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
            .map_err(|(_, error)| anyhow::anyhow!("machine catalog busy: {error}"))?;
        let mut catalog = Self::load(path)?;
        let result = update(&mut catalog)?;
        catalog.validate()?;
        let mut bytes = serde_json::to_vec_pretty(&catalog)?;
        bytes.push(b'\n');
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "machine catalog exceeds byte limit"
        );
        let candidate = parent.join(format!(".machines-{}.tmp", local_ipc::random_token()?));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)?;
        let temporary = Temporary(candidate);
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary.0, path)?;
        File::open(parent)?
            .sync_all()
            .context("catalog saved but directory sync failed; inspect before retrying")?;
        Ok(result)
    }

    pub fn add(&mut self, name: String, control: Option<Binding>) -> Result<Machine> {
        let machine = Machine {
            id: local_ipc::random_token()?,
            name,
            control,
            attachments: BTreeMap::new(),
        };
        machine.validate()?;
        ensure!(self.machines.len() < MAX_MACHINES, "machine limit exceeded");
        ensure!(
            !self
                .machines
                .iter()
                .any(|m| m.name.eq_ignore_ascii_case(&machine.name)),
            "duplicate machine name"
        );
        self.machines.push(machine.clone());
        Ok(machine)
    }

    pub fn rename(&mut self, selector: &str, name: String) -> Result<()> {
        valid_name(&name)?;
        let id = self.resolve(selector)?.id.clone();
        ensure!(
            !self
                .machines
                .iter()
                .any(|m| m.id != id && m.name.eq_ignore_ascii_case(&name)),
            "duplicate machine name"
        );
        self.by_id_mut(&id)?.name = name;
        Ok(())
    }

    pub fn control(&mut self, selector: &str, binding: Option<Binding>) -> Result<()> {
        let id = self.resolve(selector)?.id.clone();
        let machine = self.by_id_mut(&id)?;
        let mut next = machine.clone();
        next.control = binding;
        next.validate()?;
        *machine = next;
        Ok(())
    }

    pub fn bind(
        &mut self,
        selector: &str,
        workspace: String,
        binding: Option<Binding>,
    ) -> Result<()> {
        let id = self.resolve(selector)?.id.clone();
        let machine = self.by_id_mut(&id)?;
        let mut next = machine.clone();
        match binding {
            Some(binding) => {
                next.attachments.insert(workspace, binding);
            }
            None => {
                ensure!(
                    next.attachments.remove(&workspace).is_some(),
                    "unknown attachment binding"
                );
            }
        }
        next.validate()?;
        *machine = next;
        Ok(())
    }

    pub fn remove(&mut self, selector: &str) -> Result<Machine> {
        let id = self.resolve(selector)?.id.clone();
        let index = self
            .machines
            .iter()
            .position(|machine| machine.id == id)
            .context("machine disappeared")?;
        Ok(self.machines.remove(index))
    }

    fn by_id_mut(&mut self, id: &str) -> Result<&mut Machine> {
        self.machines
            .iter_mut()
            .find(|machine| machine.id == id)
            .context("machine disappeared")
    }
}

fn private_file(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == nix::unistd::geteuid().as_raw()
            && metadata.permissions().mode() & 0o077 == 0,
        "machine catalog files must be private, owned regular files"
    );
    Ok(())
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(endpoint: char) -> Binding {
        Binding {
            endpoint: endpoint.to_string().repeat(64),
            key_file: PathBuf::from("/private/key"),
            direct: None,
            relay_url: None,
        }
    }

    #[test]
    fn identity_survives_rename_and_bindings_stay_explicit() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("private/machines.json");
        let original = Catalog::edit(&path, |catalog| {
            catalog.add("build".into(), Some(binding('a')))
        })?;
        Catalog::edit(&path, |catalog| {
            catalog.rename(&original.id, "renamed".into())?;
            catalog.bind("renamed", "other".into(), Some(binding('b')))
        })?;
        let loaded = Catalog::load(&path)?;
        let machine = loaded.resolve("RENAMED")?;
        assert_eq!(machine.id, original.id);
        assert_eq!(machine.control, original.control);
        assert_eq!(machine.attachments.len(), 1);
        assert!(loaded.resolve("build").is_err());
        assert!(loaded.resolve("local").is_err());
        assert!(loaded.resolve("missing").is_err());
        Ok(())
    }

    #[test]
    fn invalid_edits_leave_original_catalog_and_grants_intact() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("private/machines.json");
        Catalog::edit(&path, |catalog| {
            catalog.add("one".into(), Some(binding('a')))
        })?;
        let before = fs::read(&path)?;
        assert!(Catalog::edit(&path, |catalog| catalog.add("ONE".into(), None)).is_err());
        assert!(
            Catalog::edit(&path, |catalog| catalog.bind(
                "one",
                "default".into(),
                Some(binding('a'))
            ))
            .is_err()
        );
        assert!(Catalog::edit(&path, |catalog| catalog.rename("one", "local".into())).is_err());
        assert!(
            Catalog::edit(&path, |catalog| catalog.bind(
                "one",
                "../other".into(),
                Some(binding('b'))
            ))
            .is_err()
        );
        assert_eq!(fs::read(&path)?, before);
        Catalog::edit(&path, |catalog| catalog.remove("one"))?;
        assert!(Catalog::load(&path)?.machines.is_empty());
        Ok(())
    }

    #[test]
    fn malformed_oversize_and_symlink_catalogs_fail_closed() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("private/machines.json");
        Catalog::edit(&path, |catalog| catalog.add("one".into(), None))?;
        fs::write(&path, b"{}")?;
        assert!(Catalog::load(&path).is_err());
        fs::write(&path, vec![b' '; MAX_BYTES as usize + 1])?;
        assert!(Catalog::load(&path).is_err());
        fs::remove_file(&path)?;
        std::os::unix::fs::symlink(root.path().join("missing"), &path)?;
        assert!(Catalog::load(&path).is_err());
        Ok(())
    }

    #[test]
    fn concurrent_edit_is_rejected_without_lost_update() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("private/machines.json");
        Catalog::edit(&path, |catalog| {
            assert!(Catalog::edit(&path, |other| other.add("racer".into(), None)).is_err());
            catalog.add("winner".into(), None)
        })?;
        assert_eq!(Catalog::load(&path)?.machines.len(), 1);
        Ok(())
    }
}
