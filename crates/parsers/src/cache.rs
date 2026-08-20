use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::util::FileSig;

pub fn cache_path(cache_dir: &Path, namespace: &str, file: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(file.to_string_lossy().as_bytes());
    let name = hex::encode(&hasher.finalize()[..16]);
    cache_dir.join(namespace).join(format!("{name}.json"))
}

pub fn load<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let raw = std::fs::read(path).ok()?;
    serde_json::from_slice(&raw).ok()
}

pub fn save<T: Serialize>(path: &Path, value: &T) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec(value) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

pub fn sig_unchanged(cached: &FileSig, current: &FileSig) -> bool {
    cached == current
}

pub fn can_append(cached: &FileSig, current: &FileSig) -> bool {
    cached.dev == current.dev
        && cached.ino == current.ino
        && cached.size > 0
        && current.size > cached.size
        && cached.mtime_ms <= current.mtime_ms
}
