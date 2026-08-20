use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncState {
    #[serde(default)]
    pub buckets: HashMap<String, String>,
    #[serde(default)]
    pub sessions: HashMap<String, String>,
}

impl SyncState {
    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read(path) else {
            return Self::default();
        };
        serde_json::from_slice(&raw).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn prune(
        &mut self,
        live_buckets: &HashSet<String>,
        live_sessions: &HashSet<String>,
        ok_sources: &HashSet<String>,
    ) {
        self.buckets.retain(|key, _| {
            let source = key.split('|').next().unwrap_or("");
            !ok_sources.contains(source) || live_buckets.contains(key)
        });
        self.sessions.retain(|key, _| {
            let source = key.split('|').next().unwrap_or("");
            !ok_sources.contains(source) || live_sessions.contains(key)
        });
    }
}
