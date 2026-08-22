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
        account_prune: &HashMap<String, (HashSet<String>, HashSet<String>)>,
    ) {
        self.buckets
            .retain(|key, _| keep_key(key, live_buckets, ok_sources, account_prune));
        self.sessions
            .retain(|key, _| keep_key(key, live_sessions, ok_sources, account_prune));
    }
}

fn keep_key(
    key: &str,
    live: &HashSet<String>,
    ok_sources: &HashSet<String>,
    account_prune: &HashMap<String, (HashSet<String>, HashSet<String>)>,
) -> bool {
    let mut parts = key.split('|');
    let source = parts.next().unwrap_or("");
    if !ok_sources.contains(source) {
        return true;
    }
    if let Some((attempted, succeeded)) = account_prune.get(source) {
        let hash = parts.next().unwrap_or("");
        if succeeded.contains(hash) {
            return live.contains(key);
        }
        if attempted.contains(hash) {
            return true;
        }
        return false;
    }
    live.contains(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_a() -> String {
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()
    }
    fn hash_b() -> String {
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()
    }

    fn key(hash: &str) -> String {
        format!("cursor|{hash}|m|p|t")
    }

    #[test]
    fn prune_keeps_failed_account_keys() {
        let mut s = SyncState::default();
        s.buckets.insert(key(&hash_a()), "h1".into());
        s.buckets.insert(key(&hash_b()), "h2".into());
        let live = HashSet::from([key(&hash_a())]);
        let ok = HashSet::from(["cursor".into()]);
        let attempted = HashSet::from([hash_a(), hash_b()]);
        let succeeded = HashSet::from([hash_a()]);
        let mut prune_map = HashMap::new();
        prune_map.insert("cursor".into(), (attempted, succeeded));
        s.prune(&live, &HashSet::new(), &ok, &prune_map);
        assert!(s.buckets.contains_key(&key(&hash_a())));
        assert!(s.buckets.contains_key(&key(&hash_b())));
    }

    #[test]
    fn prune_drops_removed_account() {
        let mut s = SyncState::default();
        s.buckets.insert(key(&hash_a()), "h1".into());
        s.buckets.insert(key(&hash_b()), "h2".into());
        let live = HashSet::from([key(&hash_a())]);
        let ok = HashSet::from(["cursor".into()]);
        let attempted = HashSet::from([hash_a()]);
        let succeeded = HashSet::from([hash_a()]);
        let mut prune_map = HashMap::new();
        prune_map.insert("cursor".into(), (attempted, succeeded));
        s.prune(&live, &HashSet::new(), &ok, &prune_map);
        assert!(s.buckets.contains_key(&key(&hash_a())));
        assert!(!s.buckets.contains_key(&key(&hash_b())));
    }

    #[test]
    fn prune_skips_source_not_in_ok() {
        let mut s = SyncState::default();
        s.buckets.insert(key(&hash_a()), "h1".into());
        s.prune(
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        assert!(s.buckets.contains_key(&key(&hash_a())));
    }
}
