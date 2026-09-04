use std::collections::BTreeMap;

#[derive(Default)]
pub struct Normalizer {
    pids: BTreeMap<u32, String>,
    paths: BTreeMap<String, String>,
    endpoints: BTreeMap<String, String>,
}

impl Normalizer {
    pub fn pid(&mut self, value: u32) -> String {
        let next = format!("process-{}", self.pids.len().saturating_add(1));
        self.pids.entry(value).or_insert(next).clone()
    }

    pub fn path(&mut self, value: &str) -> String {
        let next = format!("private-path-{}", self.paths.len().saturating_add(1));
        self.paths.entry(value.to_owned()).or_insert(next).clone()
    }

    pub fn endpoint(&mut self, value: &str) -> String {
        let next = format!("endpoint-{}", self.endpoints.len().saturating_add(1));
        self.endpoints
            .entry(value.to_owned())
            .or_insert(next)
            .clone()
    }
}
