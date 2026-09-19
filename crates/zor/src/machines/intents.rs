//! Private, bounded intent-before-dispatch evidence for every remote mutation. Loading this
//! log never creates worker requests: uncertain operations are inspected, never replayed.
use std::path::{Path, PathBuf};
use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};
use super::{catalog::{CatalogError, read_private, write_private}, supervision::ActionRecord};

pub const MAX_RECORDS: usize = 256;
pub const SUFFIX: &str = ".action-intents.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionIntent {
    pub operation: String,
    pub machine: String,
    pub control: String,
    pub record: ActionRecord,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File { version: u32, intents: Vec<ActionIntent> }

#[derive(Resource, Debug, Default)]
pub struct IntentLog { path: PathBuf, records: Vec<ActionIntent> }
impl IntentLog {
    pub fn path_beside(catalog: &Path) -> PathBuf {
        let mut name = catalog.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        name.push(SUFFIX);
        catalog.with_file_name(name)
    }
    pub fn load(catalog: &Path) -> Result<Self, CatalogError> {
        if catalog.as_os_str().is_empty() { return Ok(Self::default()); }
        let path = Self::path_beside(catalog);
        let mut records = match read_private(&path)? {
            None => Vec::new(),
            Some(bytes) => {
                let file: File = serde_json::from_slice(&bytes)?;
                if file.version != 1 || file.intents.len() > MAX_RECORDS {
                    return Err(CatalogError::Invalid("invalid action intent version or capacity".into()));
                }
                for (i, record) in file.intents.iter().enumerate() {
                    if !crate::model::valid_id(&record.operation) || file.intents[..i].iter().any(|r| r.operation == record.operation || r.record.id == record.record.id) {
                        return Err(CatalogError::Invalid("invalid or duplicate action intent identity".into()));
                    }
                }
                file.intents
            }
        };
        for intent in &mut records {
            if intent.record.phase == super::supervision::ActionPhase::Submitting {
                intent.record.phase = super::supervision::ActionPhase::Uncertain;
                intent.record.problem = Some("controller restarted; inspect remote evidence, never replay".into());
            }
        }
        Ok(Self {path, records})
    }
    pub fn records(&self) -> &[ActionIntent] { &self.records }
    pub fn find(&self, operation: &str) -> Option<&ActionIntent> { self.records.iter().find(|r| r.operation == operation) }
    /// Identical operation keys are never dispatched again, including after a lost reply.
    pub fn commit(&mut self, intent: ActionIntent) -> Result<(), CatalogError> {
        if self.find(&intent.operation).is_some() { return Err(CatalogError::Invalid("operation intent already retained; inspect its outcome, do not replay".into())); }
        if self.records.len() >= MAX_RECORDS { return Err(CatalogError::Invalid("remote action intent capacity reached".into())); }
        self.records.push(intent);
        if let Err(error) = self.save() { self.records.pop(); return Err(error); }
        Ok(())
    }
    pub fn complete(&mut self, record: &ActionRecord) -> Result<(), CatalogError> {
        let Some(index) = self.records.iter().position(|r| r.record.id == record.id) else { return Ok(()); };
        let old = std::mem::replace(&mut self.records[index].record, record.clone());
        if let Err(error) = self.save() { self.records[index].record = old; return Err(error); }
        Ok(())
    }
    fn save(&self) -> Result<(), CatalogError> {
        let value = serde_json::json!({"version":1,"intents":self.records});
        write_private(&self.path, &serde_json::to_vec_pretty(&value)?)
    }
}
