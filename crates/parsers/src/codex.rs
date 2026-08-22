use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::{ParseCtx, ParseResult, UsageAdapter};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agg::extract_sessions;
use crate::cache;
use crate::util::{
    collect_jsonl, entries_to_buckets, expand_home, file_sig, guard_hash, parse_ts,
    project_from_path, to_count_opt, FileSig, TimingEvent, UsageEntry,
};

const SOURCE: &str = ai_usage_protocol::SOURCE_CODEX;
/// Bump when token-accounting rules change so stale per-file caches are rebuilt.
const CACHE_VERSION: u32 = 1;

pub struct CodexAdapter;

impl UsageAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        SOURCE
    }

    fn detect(&self, ctx: &ParseCtx) -> Vec<PathBuf> {
        codex_homes(ctx)
            .into_iter()
            .filter(|h| h.join("sessions").is_dir() || h.join("archived_sessions").is_dir())
            .collect()
    }

    fn parse(&self, ctx: &ParseCtx) -> ParseResult {
        let mut warnings = Vec::new();
        let mut by_session: HashMap<String, Vec<FileJob>> = HashMap::new();
        for home in codex_homes(ctx) {
            for sub in ["sessions", "archived_sessions"] {
                let dir = home.join(sub);
                for path in collect_jsonl(&dir) {
                    let Some(sig) = file_sig(&path) else { continue };
                    by_session
                        .entry(path.to_string_lossy().to_string())
                        .or_default()
                        .push(FileJob { path, sig });
                }
            }
        }

        // Index by session id after a cheap header read (from cache when possible).
        let mut grouped: HashMap<String, Vec<FileJob>> = HashMap::new();
        for jobs in by_session.into_values() {
            for job in jobs {
                let header = read_header_cached(&job, &ctx.cache_dir, &mut warnings);
                let key = header
                    .session_id
                    .clone()
                    .unwrap_or_else(|| job.path.to_string_lossy().to_string());
                grouped.entry(key).or_default().push(job);
            }
        }

        let mut all_entries = Vec::new();
        let mut all_events = Vec::new();
        for (_sid, mut files) in grouped {
            files.sort_by(|a, b| {
                b.sig
                    .size
                    .cmp(&a.sig.size)
                    .then(b.sig.mtime_ms.cmp(&a.sig.mtime_ms))
                    .then(a.path.cmp(&b.path))
            });
            let Some(best) = files.into_iter().next() else {
                continue;
            };
            match parse_file(&best, &ctx.cache_dir) {
                Ok(parsed) => {
                    all_entries.extend(parsed.entries);
                    all_events.extend(parsed.events);
                }
                Err(err) => warnings.push(format!("Codex: {}: {err}", best.path.display())),
            }
        }

        ParseResult {
            buckets: entries_to_buckets(&all_entries),
            sessions: extract_sessions(&all_events),
            skipped: false,
            warnings,
            ..ParseResult::default()
        }
    }
}

struct FileJob {
    path: PathBuf,
    sig: FileSig,
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct Header {
    session_id: Option<String>,
    project: String,
    session_started_ms: Option<i64>,
    is_subagent: bool,
    forked_from_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct TokenTotals {
    input_tokens: i64,
    output_tokens: i64,
    cached_input_tokens: i64,
    reasoning_output_tokens: i64,
}

#[derive(Clone, Serialize, Deserialize)]
struct FileCache {
    #[serde(default)]
    algorithm_version: u32,
    sig: FileSig,
    header: Header,
    parsed_bytes: u64,
    guard_hash: String,
    ends_with_newline: bool,
    prev_total: Option<TokenTotals>,
    prev_cumulative_total: Option<i64>,
    turn_model: String,
    first_session_meta_seen: bool,
    entries: Vec<UsageEntry>,
    events: Vec<TimingEvent>,
}

struct ParsedFile {
    entries: Vec<UsageEntry>,
    events: Vec<TimingEvent>,
}

fn codex_homes(ctx: &ParseCtx) -> Vec<PathBuf> {
    let mut homes = Vec::new();
    if let Some(h) = &ctx.env.codex_home {
        homes.push(h.clone());
    } else if let Ok(h) = std::env::var("CODEX_HOME") {
        if !h.trim().is_empty() {
            homes.push(expand_home(&h, &ctx.home));
        }
    }
    if homes.is_empty() {
        homes.push(ctx.home.join(".codex"));
    }
    if let Some(extra) = &ctx.env.extra_codex_home {
        homes.push(extra.clone());
    }
    let mut seen = std::collections::HashSet::new();
    homes
        .into_iter()
        .filter(|h| seen.insert(std::fs::canonicalize(h).unwrap_or_else(|_| h.clone())))
        .collect()
}

fn read_header_cached(job: &FileJob, cache_dir: &Path, warnings: &mut Vec<String>) -> Header {
    let cache_file = cache::cache_path(cache_dir, "codex", &job.path);
    if let Some(hit) = cache::load::<FileCache>(&cache_file) {
        if cache::sig_unchanged(&hit.sig, &job.sig) || cache::can_append(&hit.sig, &job.sig) {
            return hit.header;
        }
    }
    match read_header(&job.path, job.sig.size) {
        Some(h) => h,
        None => {
            warnings.push(format!("Codex: no session_meta in {}", job.path.display()));
            Header {
                project: "unknown".into(),
                ..Header::default()
            }
        }
    }
}

fn read_header(path: &Path, size: u64) -> Option<Header> {
    let mut found = None;
    crate::util::read_jsonl_limited(path, Some(size).filter(|s| *s > 0), 0, &mut |obj| {
        if found.is_some() {
            return;
        }
        if obj.get("type").and_then(|v| v.as_str()) != Some("session_meta") {
            return;
        }
        let Some(payload) = obj.get("payload") else {
            return;
        };
        found = Some(header_from_payload(payload, obj.get("timestamp")));
    });
    found
}

fn header_from_payload(payload: &Value, obj_ts: Option<&Value>) -> Header {
    Header {
        session_id: payload
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        project: extract_project(payload),
        session_started_ms: payload
            .get("timestamp")
            .and_then(parse_ts)
            .or_else(|| obj_ts.and_then(parse_ts))
            .map(|t| t.timestamp_millis()),
        is_subagent: is_subagent(payload),
        forked_from_id: payload
            .get("forked_from_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    }
}

fn extract_project(meta: &Value) -> String {
    if let Some(url) = meta.pointer("/git/repository_url").and_then(|v| v.as_str()) {
        let trimmed = url.trim_end_matches(".git");
        if let Some((_, repo)) = trimmed.rsplit_once('/') {
            if !repo.is_empty() {
                return repo.to_string();
            }
        }
    }
    if let Some(cwd) = meta.get("cwd").and_then(|v| v.as_str()) {
        return project_from_path(cwd);
    }
    "unknown".into()
}

fn is_subagent(meta: &Value) -> bool {
    if meta.get("thread_source").and_then(|v| v.as_str()) == Some("subagent") {
        return true;
    }
    match meta.get("source") {
        Some(Value::String(s)) if s == "subagent" => true,
        Some(Value::Object(o)) if o.contains_key("subagent") => true,
        _ => meta.get("parent_thread_id").is_some(),
    }
}

fn is_replay(header: &Header) -> bool {
    header.is_subagent || header.forked_from_id.is_some()
}

fn cache_current(hit: &FileCache) -> bool {
    hit.algorithm_version == CACHE_VERSION
}

fn parse_file(job: &FileJob, cache_dir: &Path) -> Result<ParsedFile, String> {
    let cache_file = cache::cache_path(cache_dir, "codex", &job.path);
    let prior = cache::load::<FileCache>(&cache_file).filter(cache_current);
    if let Some(hit) = &prior {
        if cache::sig_unchanged(&hit.sig, &job.sig) {
            return Ok(ParsedFile {
                entries: hit.entries.clone(),
                events: hit.events.clone(),
            });
        }
    }

    let mut state = if let Some(hit) = prior.filter(|h| cache::can_append(&h.sig, &job.sig)) {
        if let Some((hash, ends_nl)) = guard_hash(&job.path, hit.sig.size) {
            if ends_nl && hash == hit.guard_hash {
                hit
            } else {
                fresh_state(job)
            }
        } else {
            fresh_state(job)
        }
    } else {
        fresh_state(job)
    };

    let start = state.parsed_bytes;
    let end = job.sig.size;
    if end == 0 {
        return Ok(ParsedFile {
            entries: state.entries,
            events: state.events,
        });
    }

    let path_fallback = job.path.to_string_lossy().to_string();

    crate::util::read_jsonl_limited(&job.path, Some(end), start, &mut |obj| {
        let ty = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if ty == "session_meta" {
            if !state.first_session_meta_seen {
                if let Some(payload) = obj.get("payload") {
                    state.header = header_from_payload(payload, obj.get("timestamp"));
                }
                state.first_session_meta_seen = true;
            }
        }
        let session_id = state
            .header
            .session_id
            .clone()
            .unwrap_or_else(|| path_fallback.clone());
        let project = if state.header.project.is_empty() {
            "unknown".to_string()
        } else {
            state.header.project.clone()
        };
        let replay = is_replay(&state.header);
        if ty == "turn_context" {
            if let Some(model) = obj
                .pointer("/payload/model")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                state.turn_model = model.to_string();
            }
        }

        if let Some(ts) = obj.get("timestamp").and_then(parse_ts) {
            let keep_meta = ty == "session_meta" && !replay;
            if ty != "session_meta" || keep_meta {
                let is_user = ty == "turn_context" || ty == "session_meta";
                state.events.push(TimingEvent {
                    session_id: session_id.clone(),
                    source: SOURCE.into(),
                    project: project.clone(),
                    timestamp: ts,
                    role: if is_user { "user" } else { "assistant" }.into(),
                });
            }
        }

        if replay || ty != "event_msg" {
            return;
        }
        let Some(payload) = obj.get("payload") else {
            return;
        };
        if payload.get("type").and_then(|v| v.as_str()) != Some("token_count") {
            return;
        }
        let Some(info) = payload.get("info") else {
            return;
        };

        let cumulative_total = info
            .pointer("/total_token_usage/total_tokens")
            .map(to_count_opt_value);
        let is_dup = matches!(
            (cumulative_total, state.prev_cumulative_total),
            (Some(cur), Some(prev)) if cur > 0 && cur == prev
        );
        if let Some(cur) = cumulative_total {
            state.prev_cumulative_total = Some(cur);
        }

        let curr = info.get("total_token_usage").map(token_totals);
        let mut usage = info.get("last_token_usage").map(token_totals);
        if usage.is_none() {
            if let Some(curr) = &curr {
                usage = Some(match &state.prev_total {
                    Some(prev) => {
                        let delta = TokenTotals {
                            input_tokens: curr.input_tokens - prev.input_tokens,
                            output_tokens: curr.output_tokens - prev.output_tokens,
                            cached_input_tokens: curr.cached_input_tokens
                                - prev.cached_input_tokens,
                            reasoning_output_tokens: curr.reasoning_output_tokens
                                - prev.reasoning_output_tokens,
                        };
                        if delta.input_tokens < 0
                            || delta.output_tokens < 0
                            || delta.cached_input_tokens < 0
                            || delta.reasoning_output_tokens < 0
                        {
                            curr.clone()
                        } else {
                            delta
                        }
                    }
                    None => curr.clone(),
                });
            }
        }
        if let Some(curr) = curr {
            state.prev_total = Some(curr);
        }
        if is_dup {
            return;
        }
        let Some(usage) = usage else { return };
        let Some(ts) = obj.get("timestamp").and_then(parse_ts) else {
            return;
        };
        let model = info
            .get("model")
            .and_then(|v| v.as_str())
            .or_else(|| payload.get("model").and_then(|v| v.as_str()))
            .filter(|s| !s.is_empty())
            .unwrap_or(&state.turn_model)
            .to_string();
        let cached = usage.cached_input_tokens;
        let reasoning = usage.reasoning_output_tokens;
        let input = (usage.input_tokens - cached).max(0);
        let output = (usage.output_tokens - reasoning).max(0);
        if input + output + cached + reasoning == 0 {
            return;
        }
        state.entries.push(UsageEntry {
            source: SOURCE.into(),
            model,
            project: project.clone(),
            timestamp: ts,
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: cached.max(0),
            cache_creation_input_tokens: 0,
            reasoning_output_tokens: reasoning.max(0),
        });
    });

    state.algorithm_version = CACHE_VERSION;
    state.parsed_bytes = end;
    state.sig = job.sig.clone();
    if let Some((hash, ends_nl)) = guard_hash(&job.path, end) {
        state.guard_hash = hash;
        state.ends_with_newline = ends_nl;
    }
    cache::save(&cache_file, &state);
    Ok(ParsedFile {
        entries: state.entries,
        events: state.events,
    })
}

fn to_count_opt_value(v: &Value) -> i64 {
    to_count_opt(Some(v))
}

fn token_totals(v: &Value) -> TokenTotals {
    TokenTotals {
        input_tokens: to_count_opt(v.get("input_tokens")),
        output_tokens: to_count_opt(v.get("output_tokens")),
        cached_input_tokens: to_count_opt(
            v.get("cached_input_tokens")
                .or_else(|| v.get("cache_read_input_tokens")),
        ),
        reasoning_output_tokens: to_count_opt(v.get("reasoning_output_tokens")),
    }
}

fn fresh_state(job: &FileJob) -> FileCache {
    FileCache {
        algorithm_version: CACHE_VERSION,
        sig: job.sig.clone(),
        header: Header {
            project: "unknown".into(),
            ..Header::default()
        },
        parsed_bytes: 0,
        guard_hash: String::new(),
        ends_with_newline: false,
        prev_total: None,
        prev_cumulative_total: None,
        turn_model: "unknown".into(),
        first_session_meta_seen: false,
        entries: Vec::new(),
        events: Vec::new(),
    }
}
