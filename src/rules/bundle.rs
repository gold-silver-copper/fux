use super::{RuleSet, load};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    fs,
    io::Read as _,
    path::{Path, PathBuf},
};

/// Maximum number of external TOML rule files loaded across all configured directories.
pub const MAX_RULE_FILES: usize = 256;
/// Maximum encoded size of one external TOML rule file.
pub const MAX_RULE_FILE_BYTES: usize = 1024 * 1024;
/// Maximum encoded size accepted by `zor check` for a captured fixture.
pub const MAX_FIXTURE_BYTES: usize = 4 * 1024 * 1024;

pub fn load_all(extra: &[PathBuf]) -> Result<Vec<RuleSet>> {
    load_from(
        config_root(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
        .as_deref(),
        extra,
    )
}

fn config_root(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    xdg.map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".config"))
        })
}

fn load_from(config: Option<&Path>, extra: &[PathBuf]) -> Result<Vec<RuleSet>> {
    let mut sets = BTreeMap::new();
    let set = load(
        Path::new("codex.toml"),
        include_str!("../../rules/codex.toml"),
    )?;
    sets.insert(set.id.clone(), set);
    let set = load(
        Path::new("claude.toml"),
        include_str!("../../rules/claude.toml"),
    )?;
    sets.insert(set.id.clone(), set);
    let set = load(
        Path::new("opencode.toml"),
        include_str!("../../rules/opencode.toml"),
    )?;
    sets.insert(set.id.clone(), set);
    let mut files = 0usize;
    if let Some(root) = config {
        load_dir(&root.join("zor/rules"), &mut sets, &mut files)?;
    }
    for directory in extra {
        load_dir(directory, &mut sets, &mut files)?;
    }
    Ok(sets.into_values().collect())
}

/// A reload either replaces the complete validated collection or leaves the old one intact.
pub struct Catalog {
    sets: Vec<RuleSet>,
    generation: u64,
}

impl Catalog {
    pub fn load(extra: &[PathBuf]) -> Result<Self> {
        Ok(Self {
            sets: load_all(extra)?,
            generation: 1,
        })
    }

    pub fn sets(&self) -> &[RuleSet] {
        &self.sets
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn reload(&mut self, extra: &[PathBuf]) -> Result<()> {
        self.reload_from(
            config_root(
                std::env::var_os("XDG_CONFIG_HOME"),
                std::env::var_os("HOME"),
            )
            .as_deref(),
            extra,
        )
    }

    fn reload_from(&mut self, config: Option<&Path>, extra: &[PathBuf]) -> Result<()> {
        let next = self
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("rule generation exhausted"))?;
        let candidate = load_from(config, extra)?;
        self.sets = candidate;
        self.generation = next;
        Ok(())
    }
}
fn load_dir(
    directory: &Path,
    sets: &mut BTreeMap<String, RuleSet>,
    files: &mut usize,
) -> Result<()> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read rules directory {}", directory.display()));
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        if (*files).saturating_add(paths.len()) >= MAX_RULE_FILES {
            anyhow::bail!("external rule file limit ({MAX_RULE_FILES}) exceeded");
        }
        paths.push(path);
    }
    paths.sort();
    for path in paths {
        *files += 1;
        let source = read_bounded_utf8(&path, MAX_RULE_FILE_BYTES)
            .with_context(|| format!("read {}", path.display()))?;
        let set = load(&path, &source)?;
        sets.insert(set.id.clone(), set);
    }
    Ok(())
}

pub fn read_bounded_utf8(path: &Path, limit: usize) -> Result<String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut bytes = Vec::new();
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        anyhow::bail!("input is not a regular file");
    }
    file.take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        anyhow::bail!("input exceeds {limit} bytes");
    }
    String::from_utf8(bytes).map_err(Into::into)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn bundled_rules_load_without_configuration_and_allow_replacement() {
        let sets = load_from(None, &[]).expect("bundled rules");
        let codex = sets.iter().find(|set| set.id == "codex").expect("codex");
        assert_eq!(codex.rules.len(), 5);
        let claude = sets.iter().find(|set| set.id == "claude").expect("claude");
        assert_eq!(claude.rules.len(), 3);
        assert_eq!(claude.process_names, vec!["claude"]);
        let opencode = sets
            .iter()
            .find(|set| set.id == "opencode")
            .expect("opencode");
        assert_eq!(opencode.rules.len(), 3);
        assert_eq!(opencode.process_names, vec!["opencode"]);

        let root = std::env::temp_dir().join(format!("zor-bundle-override-{}", std::process::id()));
        std::fs::create_dir(&root).expect("private directory");
        std::fs::write(root.join("codex.toml"), "id='codex'\nrules=[]").expect("override");
        let sets = load_from(None, std::slice::from_ref(&root)).expect("replacement");
        assert!(
            sets.iter()
                .find(|set| set.id == "codex")
                .expect("codex")
                .rules
                .is_empty()
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn inherited_process_names_and_agent_ids_are_validated() {
        let aliases: Vec<_> = (0..64).map(|n| format!("alias{n}")).collect();
        let source = format!(
            "id='fixture'\naliases={}\nrules=[]",
            serde_json::to_string(&aliases).expect("aliases")
        );
        assert!(load(Path::new("fixture.toml"), &source).is_err());
        let aliases: Vec<_> = aliases.into_iter().take(63).collect();
        let source = format!(
            "id='fixture'\naliases={}\nrules=[]",
            serde_json::to_string(&aliases).expect("aliases")
        );
        assert_eq!(
            load(Path::new("fixture.toml"), &source)
                .expect("bounded inherited names")
                .process_names
                .len(),
            64
        );
        assert!(load(Path::new("fixture.toml"), "id='invalid agent'\nrules=[]").is_err());
    }

    #[test]
    fn standard_config_fallback_ignores_relative_xdg_paths() {
        let home = Some(std::ffi::OsString::from("/example/home"));
        assert_eq!(
            config_root(None, home.clone()),
            Some(PathBuf::from("/example/home/.config"))
        );
        assert_eq!(
            config_root(Some("relative".into()), home.clone()),
            Some(PathBuf::from("/example/home/.config"))
        );
        assert_eq!(
            config_root(Some("/custom".into()), home),
            Some(PathBuf::from("/custom"))
        );
        assert_eq!(config_root(Some("relative".into()), None), None);
    }

    #[test]
    fn overrides_are_ordered_and_failed_reload_preserves_the_entire_catalog() {
        let root = std::env::temp_dir().join(format!("zor-atomic-rules-{}", std::process::id()));
        std::fs::create_dir(&root).expect("private test directory");
        let config = root.join("config");
        let defaults = config.join("zor/rules");
        let overrides = root.join("override");
        std::fs::create_dir_all(&defaults).expect("defaults");
        std::fs::create_dir(&overrides).expect("overrides");
        let rule = |name: &str| format!("id='fixture'\nprocess_names=['{name}']\nrules=[]\n");
        std::fs::write(defaults.join("a.toml"), rule("default")).expect("default rule");
        std::fs::write(overrides.join("a.toml"), rule("first")).expect("first override");
        std::fs::write(overrides.join("z.toml"), rule("last")).expect("last override");
        let extra = vec![overrides.clone()];
        let mut catalog = Catalog {
            sets: load_from(Some(&config), &extra).expect("load"),
            generation: 1,
        };
        assert_eq!(
            catalog
                .sets()
                .iter()
                .find(|set| set.id == "fixture")
                .expect("set")
                .process_names,
            vec!["last"]
        );
        std::fs::write(overrides.join("z.toml"), "malformed[").expect("broken update");
        assert!(catalog.reload_from(Some(&config), &extra).is_err());
        assert_eq!(catalog.generation(), 1);
        assert_eq!(
            catalog
                .sets()
                .iter()
                .find(|set| set.id == "fixture")
                .expect("retained set")
                .process_names,
            vec!["last"]
        );
        std::fs::write(overrides.join("z.toml"), rule("reloaded")).expect("valid update");
        catalog.reload_from(Some(&config), &extra).expect("reload");
        assert_eq!(catalog.generation(), 2);
        assert_eq!(
            catalog
                .sets()
                .iter()
                .find(|set| set.id == "fixture")
                .expect("new set")
                .process_names,
            vec!["reloaded"]
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn bounded_reader_rejects_oversized_input() {
        let path = std::env::temp_dir().join(format!("zor-bounded-read-{}", std::process::id()));
        std::fs::write(&path, [b'x'; 17]).expect("write");
        assert!(read_bounded_utf8(&path, 16).is_err());
        std::fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn aggregate_rule_file_count_is_bounded() {
        let directory = std::env::temp_dir().join(format!("zor-rule-count-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).expect("directory");
        std::fs::write(directory.join("extra.toml"), b"id='extra'").expect("rule");
        let mut sets = BTreeMap::new();
        let mut files = MAX_RULE_FILES;
        assert!(load_dir(&directory, &mut sets, &mut files).is_err());
        std::fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn bounded_reader_rejects_fifo_without_waiting_for_a_writer() {
        use nix::sys::stat::Mode;
        use nix::unistd::mkfifo;
        let path = std::env::temp_dir().join(format!("zor-bounded-fifo-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).expect("fifo");
        let started = std::time::Instant::now();
        assert!(read_bounded_utf8(&path, 16).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        std::fs::remove_file(path).expect("cleanup");
    }
}
