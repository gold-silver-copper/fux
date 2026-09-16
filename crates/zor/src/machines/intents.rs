//! Resume intents (multi-machine-supervision.md:98-109): before a guarded resume is dispatched
//! to a remote zor, the controller commits the original machine id, control address, service
//! incarnation, task/attempt selection, operation id and requested fux incarnation to
//! `machines.json.resume-intents.json` beside the catalog, under private permissions. A record
//! is intent evidence, never completion: it survives replies and restarts and is never
//! replayed; reusing an operation id cannot change its task, machine or requested incarnation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::catalog::{CatalogError, read_private, write_private};

pub const MAX_RECORDS: usize = 256;
pub const SUFFIX: &str = ".resume-intents.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeIntent {
    pub operation: String,
    /// Stable machine id.
    pub machine: String,
    /// The control transport's address at commit time.
    pub control: String,
    /// The remote zor incarnation the intent was taken against.
    pub instance: String,
    pub task: String,
    pub attempt: Option<u64>,
    /// The fux incarnation the caller selected explicitly.
    pub fux_instance: String,
    pub requested_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    #[serde(default)]
    intents: Vec<ResumeIntent>,
}

#[derive(Debug)]
pub enum IntentError {
    /// The operation id is retained with different intent.
    Conflict(String),
    Full,
    Store(CatalogError),
}

impl core::fmt::Display for IntentError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Conflict(op) => write!(
                f,
                "operation {op:?} is already committed with different intent; reusing an \
                 operation cannot change its task, machine or requested incarnation"
            ),
            Self::Full => write!(f, "{MAX_RECORDS} resume intents are retained; archival is unfinished"),
            Self::Store(e) => write!(f, "resume intents: {e}"),
        }
    }
}

impl core::error::Error for IntentError {}

/// Outcome of a commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Committed {
    /// Written now: the caller may dispatch once.
    New,
    /// Identical intent already retained: not replayed.
    Retained,
}

#[derive(Debug, Default)]
pub struct IntentLog {
    path: PathBuf,
    records: Vec<ResumeIntent>,
}

impl IntentLog {
    pub fn path_beside(catalog: &Path) -> PathBuf {
        let mut name = catalog.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        name.push(SUFFIX);
        catalog.with_file_name(name)
    }

    /// Loads the log beside `catalog`; a missing file is empty. An unconfigured catalog path
    /// gives a log that refuses commits.
    pub fn load(catalog: &Path) -> Result<Self, CatalogError> {
        if catalog.as_os_str().is_empty() {
            return Ok(Self::default());
        }
        let path = Self::path_beside(catalog);
        let records = match read_private(&path)? {
            Some(bytes) => {
                let file: File = serde_json::from_slice(&bytes)?;
                if file.version != 1 {
                    return Err(CatalogError::Invalid(format!(
                        "resume intents version {} is not 1",
                        file.version
                    )));
                }
                file.intents
            }
            None => Vec::new(),
        };
        Ok(Self { path, records })
    }

    pub fn records(&self) -> &[ResumeIntent] {
        &self.records
    }

    pub fn find(&self, operation: &str) -> Option<&ResumeIntent> {
        self.records.iter().find(|r| r.operation == operation)
    }

    /// Writes the intent before any dispatch. Identical intent under a retained id is
    /// `Retained` (no write, no dispatch); different intent under that id is refused.
    pub fn commit(&mut self, intent: ResumeIntent) -> Result<Committed, IntentError> {
        if let Some(existing) = self.find(&intent.operation) {
            let same = existing.machine == intent.machine
                && existing.task == intent.task
                && existing.fux_instance == intent.fux_instance
                && existing.control == intent.control;
            return if same {
                Ok(Committed::Retained)
            } else {
                Err(IntentError::Conflict(intent.operation))
            };
        }
        if self.path.as_os_str().is_empty() {
            return Err(IntentError::Store(CatalogError::Unconfigured));
        }
        if self.records.len() >= MAX_RECORDS {
            return Err(IntentError::Full);
        }
        self.records.push(intent);
        let file = File {
            version: 1,
            intents: core::mem::take(&mut self.records),
        };
        let written = serde_json::to_vec_pretty(&file)
            .map_err(CatalogError::from)
            .and_then(|bytes| write_private(&self.path, &bytes));
        self.records = file.intents;
        match written {
            Ok(()) => Ok(Committed::New),
            Err(e) => {
                self.records.pop();
                Err(IntentError::Store(e))
            }
        }
    }
}
