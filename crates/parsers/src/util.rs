use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use ai_usage_protocol::{round_to_half_hour, UsageBucket};

pub fn expand_home(value: &str, home: &Path) -> PathBuf {
    let trimmed = value.trim().trim_end_matches(['/', '\\']);
    if trimmed == "~" {
        home.to_path_buf()
    } else if let Some(rest) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
    {
        home.join(rest)
    } else {
        PathBuf::from(trimmed)
    }
}

pub fn project_from_path(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return "unknown".into();
    }
    trimmed
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

pub fn project_from_encoded_dir(name: &str) -> String {
    match urlencoding_decode(name) {
        Some(decoded) if decoded.contains('/') || decoded.contains('\\') => {
            project_from_path(&decoded)
        }
        Some(decoded) if !decoded.is_empty() => decoded,
        _ => {
            if name.is_empty() {
                "unknown".into()
            } else {
                name.to_string()
            }
        }
    }
}

fn urlencoding_decode(s: &str) -> Option<String> {
    let mut out = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let val = u8::from_str_radix(hex, 16).ok()?;
            out.push(val as char);
            i += 3;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    Some(out)
}

pub fn to_count(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_u64().map(|u| u as i64))
            .or_else(|| n.as_f64().map(|f| f.round() as i64))
            .unwrap_or(0)
            .max(0),
        Value::String(s) => s
            .parse::<f64>()
            .ok()
            .map(|f| f.round() as i64)
            .unwrap_or(0)
            .max(0),
        _ => 0,
    }
}

pub fn to_count_opt(v: Option<&Value>) -> i64 {
    v.map(to_count).unwrap_or(0)
}

pub fn parse_ts(v: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    match v {
        Value::String(s) => chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|t| t.with_timezone(&chrono::Utc))
            .or_else(|| {
                chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
                    .ok()
                    .map(|n| n.and_utc())
            }),
        Value::Number(n) => {
            let raw = n.as_f64()?;
            let ms = if raw < 1_000_000_000_000.0 {
                raw * 1000.0
            } else {
                raw
            };
            chrono::DateTime::from_timestamp_millis(ms as i64)
        }
        _ => None,
    }
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct UsageEntry {
    pub source: String,
    pub model: String,
    pub project: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub reasoning_output_tokens: i64,
    /// Empty on Cursor (account CSV has no session). Old parser caches default empty.
    #[serde(default)]
    pub session_id: String,
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct TimingEvent {
    pub session_id: String,
    pub source: String,
    pub project: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub role: String,
}

pub fn entries_to_buckets(entries: &[UsageEntry]) -> Vec<UsageBucket> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, UsageBucket> = BTreeMap::new();
    for e in entries {
        let start = round_to_half_hour(e.timestamp);
        let key = format!(
            "{}|{}|{}|{}",
            e.source,
            e.model,
            e.project,
            start.to_rfc3339()
        );
        let b = map.entry(key).or_insert_with(|| UsageBucket {
            source: e.source.clone(),
            model: e.model.clone(),
            project: e.project.clone(),
            bucket_start: start,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 0,
            account_hash: String::new(),
            account_label: String::new(),
        });
        b.input_tokens += e.input_tokens;
        b.output_tokens += e.output_tokens;
        b.cache_read_input_tokens += e.cache_read_input_tokens;
        b.cache_creation_input_tokens += e.cache_creation_input_tokens;
        b.reasoning_output_tokens += e.reasoning_output_tokens;
    }
    map.into_values().map(|b| b.normalize()).collect()
}

pub fn attach_session_id(entries: &mut [UsageEntry], session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    for e in entries {
        if e.session_id.is_empty() {
            e.session_id = session_id.to_string();
        }
    }
}

pub fn read_jsonl_limited(
    path: &Path,
    max_bytes: Option<u64>,
    start: u64,
    on_obj: &mut dyn FnMut(Value),
) {
    use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let mut file = file;
    if start > 0 {
        if file.seek(SeekFrom::Start(start)).is_err() {
            return;
        }
    }
    let reader: Box<dyn Read> = match max_bytes {
        Some(end) if end > start => Box::new(file.take(end.saturating_sub(start))),
        Some(_) => return,
        None => Box::new(file),
    };
    let buf = BufReader::new(reader);
    for line in buf.lines() {
        let Ok(line) = line else { continue };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(obj) = serde_json::from_str::<Value>(line) {
            on_obj(obj);
        }
    }
}

pub fn read_json_value(path: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn collect_jsonl(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_jsonl_into(dir, &mut out);
    out
}

fn collect_jsonl_into(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() {
            collect_jsonl_into(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

pub fn file_sig(path: &Path) -> Option<FileSig> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileSig {
        size: meta.len(),
        mtime_ms: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        ino: inode(&meta),
        dev: device(&meta),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct FileSig {
    pub size: u64,
    pub mtime_ms: u64,
    pub ino: u64,
    pub dev: u64,
}

#[cfg(unix)]
fn inode(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
fn inode(_: &std::fs::Metadata) -> u64 {
    0
}

#[cfg(unix)]
fn device(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.dev()
}

#[cfg(not(unix))]
fn device(_: &std::fs::Metadata) -> u64 {
    0
}

pub fn guard_hash(path: &Path, size: u64) -> Option<(String, bool)> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom};
    if size == 0 {
        return Some((String::new(), true));
    }
    let mut file = std::fs::File::open(path).ok()?;
    let len = size.min(4096);
    file.seek(SeekFrom::Start(size - len)).ok()?;
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf).ok()?;
    let ends_nl = buf.last().copied() == Some(b'\n');
    let hash = hex::encode(&Sha256::digest(&buf)[..10]);
    Some((hash, ends_nl))
}
