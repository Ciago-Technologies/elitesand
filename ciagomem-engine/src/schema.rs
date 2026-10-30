//! Deterministic version transitions. Projection is pure; installation is journal-gated by Engine.
use crate::{Error, Fields, Record, Result};
use std::collections::BTreeMap;
pub type Upcaster = fn(&Fields) -> Result<Fields>;
pub struct Schemas {
    steps: BTreeMap<u32, Upcaster>,
    latest: u32,
}
impl Schemas {
    pub fn new() -> Self {
        Self {
            steps: BTreeMap::new(),
            latest: 1,
        }
    }
    pub fn register(&mut self, from: u32, step: Upcaster) -> Result<()> {
        if from != self.latest || from == u32::MAX {
            return Err(Error::MissingSchema);
        }
        self.steps.insert(from, step);
        self.latest = from + 1;
        Ok(())
    }
    pub fn latest(&self) -> u32 {
        self.latest
    }
    pub fn project(&self, r: &Record) -> Result<Record> {
        if r.schema_version == 0 || r.schema_version > self.latest {
            return Err(Error::MissingSchema);
        }
        let mut out = r.clone();
        while out.schema_version < self.latest {
            out.fields = self
                .steps
                .get(&out.schema_version)
                .ok_or(Error::MissingSchema)?(&out.fields)?;
            out.schema_version += 1;
        }
        // Identity, provenance, reader permissions and embeddings are never set by upcasters.
        Ok(out)
    }
}

impl Default for Schemas {
    fn default() -> Self {
        Self::new()
    }
}
