use std::path::{Path, PathBuf};

use crate::{ParseCtx, ParseResult, UsageAdapter};

use crate::agg::extract_sessions;
use crate::cache;
use crate::util::{
    collect_jsonl, entries_to_buckets, expand_home, file_sig, parse_ts, project_from_path,
    to_count_opt, FileSig, TimingEvent, UsageEntry,
};

const SOURCE: &str = ai_usage_protocol::SOURCE_CLAUDE_CODE;

pub struct ClaudeCodeAdapter;

impl UsageAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        SOURCE
    }

    fn detect(&self, ctx: &ParseCtx) -> Vec<PathBuf> {
        claude_roots(ctx)
            .into_iter()
            .filter(|root| root.join("projects").is_dir() || root.join("transcripts").is_dir())
            .collect()
    }

    fn parse(&self, ctx: &ParseCtx) -> ParseResult {
        let mut warnings = Vec::new();
        let roots = claude_roots(ctx);
        let mut groups: std::collections::HashMap<String, Vec<Candidate>> =
            std::collections::HashMap::new();
        for root in &roots {
            collect_candidates(root, "projects", true, &mut groups, &mut warnings);
        }
        let mut project_ids = std::collections::HashSet::new();
        let mut entries_by_key: std::collections::HashMap<String, ScoredEntry> =
            std::collections::HashMap::new();
        let mut anonymous = Vec::new();
        let mut events = Vec::new();

        for (session_id, mut cands) in groups {
            cands.sort_by(|a, b| {
                b.size
                    .cmp(&a.size)
                    .then(b.mtime_ms.cmp(&a.mtime_ms))
                    .then(a.path.cmp(&b.path))
            });
            if let Some(parsed) = scan_best(&cands, true, &ctx.cache_dir, &mut warnings) {
                project_ids.insert(session_id);
                events.extend(parsed.events);
                for e in parsed.entries {
                    merge_entry(&mut entries_by_key, &mut anonymous, e);
                }
            }
        }

        let mut transcripts: std::collections::HashMap<String, Vec<Candidate>> =
            std::collections::HashMap::new();
        for root in &roots {
            collect_candidates(root, "transcripts", false, &mut transcripts, &mut warnings);
        }
        for (session_id, mut cands) in transcripts {
            if project_ids.contains(&session_id) {
                continue;
            }
            cands.sort_by(|a, b| b.size.cmp(&a.size).then(a.path.cmp(&b.path)));
            if let Some(parsed) = scan_best(&cands, false, &ctx.cache_dir, &mut warnings) {
                events.extend(parsed.events);
            }
        }

        let entries: Vec<UsageEntry> = anonymous
            .into_iter()
            .chain(entries_by_key.into_values())
            .map(|s| s.entry)
            .collect();

        ParseResult {
            buckets: entries_to_buckets(&entries),
            sessions: extract_sessions(&events),
            skipped: false,
            warnings,
        }
    }
}

#[derive(Clone)]
struct Candidate {
    path: PathBuf,
    session_id: String,
    size: u64,
    mtime_ms: u64,
    fallback_project: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct FileCache {
    sig: FileSig,
    project: String,
    entries: Vec<ScoredEntry>,
    events: Vec<TimingEvent>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ScoredEntry {
    dedupe_key: Option<String>,
    usage_score: i64,
    entry: UsageEntry,
}

struct ParsedFile {
    entries: Vec<ScoredEntry>,
    events: Vec<TimingEvent>,
}

fn claude_roots(ctx: &ParseCtx) -> Vec<PathBuf> {
    if !ctx.env.claude_dirs.is_empty() {
        return ctx.env.claude_dirs.clone();
    }
    let mut roots = vec![ctx.home.join(".claude")];
    if let Some(configured) = &ctx.env.claude_config_dir {
        roots.push(expand_home(&configured.to_string_lossy(), &ctx.home));
    } else if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            roots.push(expand_home(&dir, &ctx.home));
        }
    }
    if let Ok(entries) = std::fs::read_dir(&ctx.home) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".claude-") {
                let candidate = ctx.home.join(name.as_ref());
                if candidate.join("projects").is_dir() || candidate.join("transcripts").is_dir() {
                    roots.push(candidate);
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::new();
    for root in roots {
        let key = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        if seen.insert(key) {
            unique.push(root);
        }
    }
    unique
}

fn collect_candidates(
    root: &Path,
    dir_name: &str,
    is_projects: bool,
    groups: &mut std::collections::HashMap<String, Vec<Candidate>>,
    warnings: &mut Vec<String>,
) {
    let base = root.join(dir_name);
    for path in collect_jsonl(&base) {
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(err) => {
                warnings.push(format!("Claude Code: cannot stat {}: {err}", path.display()));
                continue;
            }
        };
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let fallback = if is_projects {
            project_from_relative(&path, &base)
        } else {
            "unknown".into()
        };
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        groups.entry(session_id.clone()).or_default().push(Candidate {
            path,
            session_id,
            size: meta.len(),
            mtime_ms,
            fallback_project: fallback,
        });
    }
}

fn project_from_relative(file: &Path, projects_dir: &Path) -> String {
    let rel = file.strip_prefix(projects_dir).ok();
    let first = rel.and_then(|r| r.components().next());
    let Some(std::path::Component::Normal(seg)) = first else {
        return "unknown".into();
    };
    let s = seg.to_string_lossy();
    s.rsplit('-').next().filter(|p| !p.is_empty()).unwrap_or("unknown").to_string()
}

fn scan_best(
    cands: &[Candidate],
    with_usage: bool,
    cache_dir: &Path,
    warnings: &mut Vec<String>,
) -> Option<ParsedFile> {
    for cand in cands {
        match scan_candidate(cand, with_usage, cache_dir) {
            Ok(parsed) => return Some(parsed),
            Err(err) => warnings.push(format!("Claude Code: cannot read {}: {err}", cand.path.display())),
        }
    }
    None
}

fn scan_candidate(cand: &Candidate, with_usage: bool, cache_dir: &Path) -> Result<ParsedFile, String> {
    let sig = file_sig(&cand.path).ok_or_else(|| "missing file".to_string())?;
    let cache_file = cache::cache_path(cache_dir, "claude", &cand.path);
    if let Some(hit) = cache::load::<FileCache>(&cache_file) {
        if cache::sig_unchanged(&hit.sig, &sig) {
            return Ok(ParsedFile {
                entries: hit.entries,
                events: hit.events,
            });
        }
    }

    let mut session_project = cand.fallback_project.clone();
    let mut found_cwd = false;
    let mut last_model: Option<String> = None;
    let mut entries = Vec::new();
    let mut events = Vec::new();
    let end = if cand.size == 0 { None } else { Some(cand.size) };
    crate::util::read_jsonl_limited(&cand.path, end, 0, &mut |obj| {
        if !found_cwd {
            if let Some(cwd) = obj.get("cwd").and_then(|v| v.as_str()) {
                if !cwd.trim().is_empty() {
                    session_project = project_from_path(cwd);
                    found_cwd = true;
                }
            }
        }
        if let Some(event) = timing_event(&obj, &cand.session_id, &session_project) {
            events.push(event);
        }
        if !with_usage {
            return;
        }
        if obj.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            return;
        }
        let Some(usage) = obj.get("message").and_then(|m| m.get("usage")) else {
            return;
        };
        let Some(ts) = obj.get("timestamp").and_then(parse_ts) else {
            return;
        };
        let raw_model = obj
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s != "<synthetic>");
        if let Some(m) = &raw_model {
            last_model = Some(m.clone());
        }
        let model = raw_model
            .or_else(|| last_model.clone())
            .unwrap_or_else(|| "claude-unknown".into());
        let cache_write = cache_creation_tokens(usage);
        let input = to_count_opt(usage.get("input_tokens"));
        let output = to_count_opt(usage.get("output_tokens"));
        let cache_read = to_count_opt(usage.get("cache_read_input_tokens"));
        let score = input + output + cache_write + cache_read;
        if score == 0 {
            return;
        }
        entries.push(ScoredEntry {
            dedupe_key: usage_dedupe_key(&obj),
            usage_score: score,
            entry: UsageEntry {
                source: SOURCE.into(),
                model,
                project: session_project.clone(),
                timestamp: ts,
                input_tokens: input,
                output_tokens: output,
                cache_read_input_tokens: cache_read,
                cache_creation_input_tokens: cache_write,
                reasoning_output_tokens: 0,
            },
        });
    });
    for e in &mut entries {
        e.entry.project = session_project.clone();
    }
    for e in &mut events {
        e.project = session_project.clone();
    }
    cache::save(
        &cache_file,
        &FileCache {
            sig,
            project: session_project,
            entries: entries.clone(),
            events: events.clone(),
        },
    );
    Ok(ParsedFile { entries, events })
}

fn cache_creation_tokens(usage: &serde_json::Value) -> i64 {
    let direct = to_count_opt(usage.get("cache_creation_input_tokens"));
    let breakdown = usage.get("cache_creation").cloned().unwrap_or(serde_json::Value::Null);
    let split = to_count_opt(breakdown.get("ephemeral_5m_input_tokens"))
        + to_count_opt(breakdown.get("ephemeral_1h_input_tokens"));
    direct.max(split)
}

fn usage_dedupe_key(obj: &serde_json::Value) -> Option<String> {
    let message_id = obj
        .get("message")
        .and_then(|m| m.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let request_id = obj
        .get("requestId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if !message_id.is_empty() || !request_id.is_empty() {
        return Some(format!("call:{message_id}\u{0}{request_id}"));
    }
    obj.get("uuid")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn merge_entry(
    by_key: &mut std::collections::HashMap<String, ScoredEntry>,
    anonymous: &mut Vec<ScoredEntry>,
    entry: ScoredEntry,
) {
    let Some(key) = &entry.dedupe_key else {
        anonymous.push(entry);
        return;
    };
    match by_key.get(key) {
        Some(cur) if cur.usage_score >= entry.usage_score => {}
        _ => {
            by_key.insert(key.clone(), entry);
        }
    }
}

fn timing_event(obj: &serde_json::Value, session_id: &str, project: &str) -> Option<TimingEvent> {
    let ty = obj.get("type").and_then(|v| v.as_str())?;
    if !matches!(ty, "user" | "assistant" | "tool_use" | "tool_result") {
        return None;
    }
    let ts = obj.get("timestamp").and_then(parse_ts)?;
    Some(TimingEvent {
        session_id: session_id.to_string(),
        source: SOURCE.into(),
        project: project.to_string(),
        timestamp: ts,
        role: if ty == "user" { "user" } else { "assistant" }.into(),
    })
}
