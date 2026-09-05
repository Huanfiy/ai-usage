use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agg::extract_sessions;
use crate::cache;
use crate::util::{
    entries_to_buckets, expand_home, file_sig, guard_hash, parse_ts, project_from_path,
    to_count_opt, FileSig, TimingEvent, UsageEntry,
};
use crate::{ParseCtx, ParseResult, UsageAdapter};

const SOURCE: &str = ai_usage_protocol::SOURCE_PI;
const CACHE_VERSION: u32 = 1;

pub struct PiAdapter;

impl UsageAdapter for PiAdapter {
    fn id(&self) -> &'static str {
        SOURCE
    }

    fn detect(&self, ctx: &ParseCtx) -> Vec<PathBuf> {
        let root = session_dir(ctx);
        if root.is_dir() {
            vec![root]
        } else {
            Vec::new()
        }
    }

    fn parse(&self, ctx: &ParseCtx) -> ParseResult {
        let mut warnings = Vec::new();
        let mut paths = Vec::new();
        for root in self.detect(ctx) {
            collect_files(&root, &mut paths, &mut warnings);
        }
        let mut groups: BTreeMap<String, Vec<FileJob>> = BTreeMap::new();
        let mut seen = HashSet::new();
        for path in paths {
            let path = std::fs::canonicalize(&path).unwrap_or(path);
            if !seen.insert(path.clone()) {
                continue;
            }
            let Some(sig) = file_sig(&path) else {
                warnings.push(format!("Pi: cannot stat {}", path.display()));
                continue;
            };
            // Read only the header, even on cache hits. Besides grouping copies,
            // this detects a changed identity/fork marker on a growing file.
            match read_header(&path, sig.size) {
                Ok(header) => groups
                    .entry(header.session_id.clone())
                    .or_default()
                    .push(FileJob { path, sig, header }),
                Err(err) => warnings.push(format!("Pi: {}: {err}", path.display())),
            }
        }

        let mut entries = Vec::new();
        let mut events = Vec::new();
        for mut files in groups.into_values() {
            files.sort_by(|a, b| {
                b.sig
                    .size
                    .cmp(&a.sig.size)
                    .then(b.sig.mtime_ms.cmp(&a.sig.mtime_ms))
                    .then(a.path.cmp(&b.path))
            });
            let Some(job) = files.first() else { continue };
            match parse_file(job, &ctx.cache_dir) {
                Ok(parsed) => {
                    if parsed.malformed_lines > 0 {
                        warnings.push(format!(
                            "Pi: {}: 跳过 {} 条损坏记录",
                            job.path.display(),
                            parsed.malformed_lines
                        ));
                    }
                    for record in parsed.records.into_values() {
                        entries.extend(record.usage);
                        events.extend(record.event);
                    }
                }
                Err(err) => warnings.push(format!("Pi: {}: {err}", job.path.display())),
            }
        }
        ParseResult {
            buckets: entries_to_buckets(&entries),
            sessions: extract_sessions(&events, &entries),
            warnings,
            ..ParseResult::default()
        }
    }
}

fn session_dir(ctx: &ParseCtx) -> PathBuf {
    resolve_session_dir(
        ctx,
        std::env::var("PI_CODING_AGENT_SESSION_DIR").ok().as_deref(),
        std::env::var("PI_CODING_AGENT_DIR").ok().as_deref(),
    )
}

fn resolve_session_dir(
    ctx: &ParseCtx,
    env_session: Option<&str>,
    env_agent: Option<&str>,
) -> PathBuf {
    if let Some(dir) = &ctx.env.pi_session_dir {
        return expand_home(&dir.to_string_lossy(), &ctx.home);
    }
    if let Some(dir) = &ctx.env.pi_agent_dir {
        return expand_home(&dir.to_string_lossy(), &ctx.home).join("sessions");
    }
    if let Some(dir) = env_session.filter(|s| !s.trim().is_empty()) {
        return expand_home(dir, &ctx.home);
    }
    if let Some(dir) = env_agent.filter(|s| !s.trim().is_empty()) {
        return expand_home(dir, &ctx.home).join("sessions");
    }
    ctx.home.join(".pi/agent/sessions")
}

fn collect_files(dir: &Path, paths: &mut Vec<PathBuf>, warnings: &mut Vec<String>) {
    let result = (|| -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                collect_files(&path, paths, warnings);
            } else if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                paths.push(path);
            }
        }
        Ok(())
    })();
    if let Err(err) = result {
        warnings.push(format!("Pi: cannot scan {}: {err}", dir.display()));
    }
}

struct FileJob {
    path: PathBuf,
    sig: FileSig,
    header: Header,
}

// Store only accounting metadata, never cwd, parent paths or conversation text.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Header {
    version: u64,
    session_id: String,
    project: String,
    replay: bool,
}

fn read_header(path: &Path, size: u64) -> Result<Header, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file.take(size));
    let mut line = Vec::new();
    loop {
        line.clear();
        reader
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if line.last() != Some(&b'\n') {
            return Err("missing or incomplete session header".into());
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let obj: Value =
            serde_json::from_slice(&line).map_err(|_| "invalid session header".to_string())?;
        if obj.get("type").and_then(Value::as_str) != Some("session") {
            return Err("first entry is not a session header".into());
        }
        let version = obj.get("version").unwrap_or(&Value::from(1)).as_u64();
        let Some(version @ 1..=3) = version else {
            return Err("unsupported session format version".into());
        };
        let session_id = nonempty(obj.get("id"))
            .ok_or_else(|| "session header has no id".to_string())?
            .to_string();
        let replay = match obj.get("parentSession") {
            None | Some(Value::Null) => false,
            Some(Value::String(s)) => !s.trim().is_empty(),
            Some(_) => true, // Malformed ancestry must not inflate accounting.
        };
        return Ok(Header {
            version,
            session_id,
            project: obj
                .get("cwd")
                .and_then(Value::as_str)
                .map(project_from_path)
                .unwrap_or_else(|| "unknown".into()),
            replay,
        });
    }
}

#[derive(Serialize, Deserialize)]
struct FileCache {
    #[serde(default)]
    algorithm_version: u32,
    sig: FileSig,
    header: Header,
    /// Committed LF-delimited prefix only; a partially written tail is retried.
    parsed_bytes: u64,
    guard_hash: String,
    malformed_lines: usize,
    records: BTreeMap<String, Record>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    usage: Option<UsageEntry>,
    event: Option<TimingEvent>,
}

impl Record {
    fn score(&self) -> i64 {
        self.usage.as_ref().map_or(0, |u| {
            u.input_tokens
                + u.output_tokens
                + u.cache_read_input_tokens
                + u.cache_creation_input_tokens
                + u.reasoning_output_tokens
        })
    }
}

fn parse_file(job: &FileJob, cache_dir: &Path) -> Result<FileCache, String> {
    let cache_file = cache::cache_path(cache_dir, SOURCE, &job.path);
    let prior = cache::load::<FileCache>(&cache_file).filter(|h| {
        h.algorithm_version == CACHE_VERSION
            && h.header == job.header
            && h.parsed_bytes <= h.sig.size
    });
    if let Some(hit) = prior.as_ref() {
        if cache::sig_unchanged(&hit.sig, &job.sig) {
            return Ok(prior.unwrap());
        }
    }
    let mut state = prior
        .filter(|h| {
            cache::can_append(&h.sig, &job.sig)
                && guard_hash(&job.path, h.parsed_bytes)
                    .is_some_and(|(hash, nl)| nl && hash == h.guard_hash)
        })
        .unwrap_or_else(|| FileCache {
            algorithm_version: CACHE_VERSION,
            sig: job.sig.clone(),
            header: job.header.clone(),
            parsed_bytes: 0,
            guard_hash: String::new(),
            malformed_lines: 0,
            records: BTreeMap::new(),
        });

    read_tail(job, &mut state).map_err(|e| e.to_string())?;
    let (hash, _) = guard_hash(&job.path, state.parsed_bytes)
        .ok_or_else(|| "file changed while reading".to_string())?;
    state.guard_hash = hash;
    state.sig = job.sig.clone();
    cache::save(&cache_file, &state);
    Ok(state)
}

fn read_tail(job: &FileJob, state: &mut FileCache) -> std::io::Result<()> {
    let mut file = std::fs::File::open(&job.path)?;
    file.seek(SeekFrom::Start(state.parsed_bytes))?;
    let mut reader = BufReader::new(file.take(job.sig.size - state.parsed_bytes));
    let mut line = Vec::new();
    loop {
        line.clear();
        reader.read_until(b'\n', &mut line)?;
        if line.last() != Some(&b'\n') {
            break;
        }
        let start = state.parsed_bytes;
        state.parsed_bytes += line.len() as u64;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let obj: Value = match serde_json::from_slice(&line) {
            Ok(obj) => obj,
            Err(_) => {
                state.malformed_lines += 1;
                continue;
            }
        };
        let Some(record) = parse_record(&obj, &state.header) else {
            continue;
        };
        let key = match nonempty(obj.get("id")) {
            Some(id) => id.to_string(),
            None if state.header.version == 1 => format!("@{start}"),
            None => {
                state.malformed_lines += 1;
                continue;
            }
        };
        state
            .records
            .entry(key)
            .and_modify(|prev| {
                // Repeated entries are one message, not a second request. Keep the
                // original timing, but allow a later, fuller usage snapshot to win.
                if record.score() > prev.score() {
                    prev.usage = record.usage.clone();
                }
            })
            .or_insert(record);
    }
    Ok(())
}

fn parse_record(obj: &Value, header: &Header) -> Option<Record> {
    let ty = obj.get("type")?.as_str()?;
    let message = obj.get("message");
    let timestamp = obj.get("timestamp").and_then(parse_ts).or_else(|| {
        message
            .and_then(|m| m.get("timestamp"))
            .and_then(Value::as_i64)
            .and_then(chrono::DateTime::from_timestamp_millis)
    })?;
    let mut event = None;
    let (usage, model) = match ty {
        "message" => {
            let message = message?;
            let role = message.get("role")?.as_str()?;
            if !matches!(role, "user" | "assistant" | "toolResult") {
                return None;
            }
            event = Some(TimingEvent {
                session_id: header.session_id.clone(),
                source: SOURCE.into(),
                project: header.project.clone(),
                timestamp,
                role: role.into(),
            });
            let count = role == "assistant"
                && message.get("provider").and_then(Value::as_str) != Some("cursor-agent")
                && message.get("stopReason").and_then(Value::as_str) != Some("pending");
            (
                count.then(|| message.get("usage")).flatten(),
                nonempty(message.get("responseModel"))
                    .or_else(|| nonempty(message.get("model")))
                    .unwrap_or("unknown"),
            )
        }
        "compaction" | "branch_summary" => (obj.get("usage"), "unknown"),
        _ => return None,
    };
    let usage = usage
        .filter(|u| u.is_object() && !header.replay)
        .and_then(|u| {
            let input = to_count_opt(u.get("input"));
            let output = to_count_opt(u.get("output"));
            let read = to_count_opt(u.get("cacheRead"));
            let write = to_count_opt(u.get("cacheWrite"));
            let reasoning = to_count_opt(u.get("reasoning")).min(output);
            let total = input
                .checked_add(output)?
                .checked_add(read)?
                .checked_add(write)?;
            (total > 0).then(|| UsageEntry {
                source: SOURCE.into(),
                model: model.to_string(),
                project: header.project.clone(),
                timestamp,
                input_tokens: input,
                output_tokens: output - reasoning,
                cache_read_input_tokens: read,
                cache_creation_input_tokens: write,
                reasoning_output_tokens: reasoning,
                session_id: header.session_id.clone(),
            })
        });
    if event.is_none() && usage.is_none() {
        return None;
    }
    Some(Record { usage, event })
}

fn nonempty(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_directory_precedence_without_mutating_process_environment() {
        let mut ctx = ParseCtx::new("/home/test".into(), "/cache".into());
        assert_eq!(
            resolve_session_dir(&ctx, None, None),
            ctx.home.join(".pi/agent/sessions")
        );
        assert_eq!(
            resolve_session_dir(&ctx, Some("  "), Some("")),
            ctx.home.join(".pi/agent/sessions")
        );
        assert_eq!(
            resolve_session_dir(&ctx, None, Some("~/custom")),
            ctx.home.join("custom/sessions")
        );
        assert_eq!(
            resolve_session_dir(&ctx, Some("~/logs"), Some("/other")),
            ctx.home.join("logs")
        );
        ctx.env.pi_agent_dir = Some("~/explicit".into());
        assert_eq!(
            resolve_session_dir(&ctx, Some("/env"), None),
            ctx.home.join("explicit/sessions")
        );
        ctx.env.pi_session_dir = Some("~/explicit-logs".into());
        assert_eq!(
            resolve_session_dir(&ctx, Some("/env"), None),
            ctx.home.join("explicit-logs")
        );
    }
}
