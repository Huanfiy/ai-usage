use std::path::{Path, PathBuf};

use crate::{ParseCtx, ParseResult, UsageAdapter};
use serde_json::Value;

use crate::agg::extract_sessions;
use crate::cache;
use crate::util::{
    entries_to_buckets, expand_home, file_sig, parse_ts, project_from_encoded_dir,
    project_from_path, read_json_value, to_count_opt, FileSig, TimingEvent, UsageEntry,
};

const SOURCE: &str = ai_usage_protocol::SOURCE_GROK;

pub struct GrokAdapter;

impl UsageAdapter for GrokAdapter {
    fn id(&self) -> &'static str {
        SOURCE
    }

    fn detect(&self, ctx: &ParseCtx) -> Vec<PathBuf> {
        let dir = grok_sessions_dir(ctx);
        if dir.is_dir() {
            vec![dir]
        } else {
            Vec::new()
        }
    }

    fn parse(&self, ctx: &ParseCtx) -> ParseResult {
        let sessions_dir = grok_sessions_dir(ctx);
        let mut entries = Vec::new();
        let mut events = Vec::new();
        let mut warnings = Vec::new();
        for session in list_sessions(&sessions_dir) {
            match parse_session(&session, &ctx.cache_dir) {
                Ok(parsed) => {
                    entries.extend(parsed.0);
                    events.extend(parsed.1);
                }
                Err(err) => warnings.push(format!("Grok: {}: {err}", session.path.display())),
            }
        }
        ParseResult {
            buckets: entries_to_buckets(&entries),
            sessions: extract_sessions(&events),
            skipped: false,
            warnings,
            ..ParseResult::default()
        }
    }
}

struct SessionDir {
    id: String,
    path: PathBuf,
    project_fallback: String,
}

fn grok_sessions_dir(ctx: &ParseCtx) -> PathBuf {
    if let Some(h) = &ctx.env.grok_home {
        return h.join("sessions");
    }
    if let Ok(h) = std::env::var("GROK_HOME") {
        if !h.trim().is_empty() {
            return expand_home(&h, &ctx.home).join("sessions");
        }
    }
    ctx.home.join(".grok").join("sessions")
}

fn list_sessions(sessions_dir: &Path) -> Vec<SessionDir> {
    let mut out = Vec::new();
    let groups = match std::fs::read_dir(sessions_dir) {
        Ok(g) => g,
        Err(_) => return out,
    };
    for group in groups.flatten() {
        let group_path = group.path();
        if !group_path.is_dir() {
            continue;
        }
        let name = group.file_name().to_string_lossy().to_string();
        let fallback = project_from_encoded_dir(&name);
        let children = match std::fs::read_dir(&group_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for child in children.flatten() {
            let session_path = child.path();
            if !session_path.is_dir() {
                continue;
            }
            if !session_path.join("summary.json").is_file()
                && !session_path.join("updates.jsonl").is_file()
            {
                continue;
            }
            out.push(SessionDir {
                id: child.file_name().to_string_lossy().to_string(),
                path: session_path,
                project_fallback: fallback.clone(),
            });
        }
    }
    out
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct FileCache {
    sig: FileSig,
    entries: Vec<UsageEntry>,
    events: Vec<TimingEvent>,
}

fn parse_session(
    session: &SessionDir,
    cache_dir: &Path,
) -> Result<(Vec<UsageEntry>, Vec<TimingEvent>), String> {
    let updates = session.path.join("updates.jsonl");
    let summary_path = session.path.join("summary.json");
    let sig = file_sig(&updates)
        .or_else(|| file_sig(&summary_path))
        .ok_or_else(|| "missing files".to_string())?;
    let cache_file = cache::cache_path(cache_dir, "grok", &session.path);
    if let Some(hit) = cache::load::<FileCache>(&cache_file) {
        if cache::sig_unchanged(&hit.sig, &sig) {
            return Ok((hit.entries, hit.events));
        }
    }

    let summary = read_json_value(&summary_path).unwrap_or(Value::Null);
    let cwd = summary
        .pointer("/info/cwd")
        .or_else(|| summary.get("git_root_dir"))
        .and_then(|v| v.as_str());
    let project = cwd
        .map(project_from_path)
        .unwrap_or_else(|| session.project_fallback.clone());
    let fallback_model = summary
        .get("current_model_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let mut entries = Vec::new();
    let mut events = Vec::new();
    let mut saw_msg = false;
    if updates.is_file() {
        let size = std::fs::metadata(&updates).map(|m| m.len()).unwrap_or(0);
        crate::util::read_jsonl_limited(&updates, Some(size).filter(|s| *s > 0), 0, &mut |obj| {
            let Some(update) = obj.pointer("/params/update") else {
                return;
            };
            let kind = update
                .get("sessionUpdate")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let ts = obj.get("timestamp").and_then(parse_ts);
            if kind == "turn_completed" {
                if let Some(ts) = ts {
                    emit_turn_usage(
                        &mut entries,
                        update.get("usage"),
                        &project,
                        ts,
                        &fallback_model,
                    );
                }
            }
            let Some(ts) = ts else { return };
            if kind == "user_message_chunk" {
                saw_msg = true;
                events.push(TimingEvent {
                    session_id: session.id.clone(),
                    source: SOURCE.into(),
                    project: project.clone(),
                    timestamp: ts,
                    role: "user".into(),
                });
            } else if kind == "agent_message_chunk" || kind == "turn_completed" {
                saw_msg = true;
                events.push(TimingEvent {
                    session_id: session.id.clone(),
                    source: SOURCE.into(),
                    project: project.clone(),
                    timestamp: ts,
                    role: "assistant".into(),
                });
            }
        });
    }

    if !saw_msg {
        let events_path = session.path.join("events.jsonl");
        if events_path.is_file() {
            let size = std::fs::metadata(&events_path)
                .map(|m| m.len())
                .unwrap_or(0);
            crate::util::read_jsonl_limited(
                &events_path,
                Some(size).filter(|s| *s > 0),
                0,
                &mut |obj| {
                    let ts = obj
                        .get("ts")
                        .and_then(parse_ts)
                        .or_else(|| obj.get("timestamp").and_then(parse_ts));
                    let Some(ts) = ts else { return };
                    let ty = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if ty == "turn_started" {
                        events.push(TimingEvent {
                            session_id: session.id.clone(),
                            source: SOURCE.into(),
                            project: project.clone(),
                            timestamp: ts,
                            role: "user".into(),
                        });
                    } else if ty == "turn_ended" || ty == "first_token" {
                        events.push(TimingEvent {
                            session_id: session.id.clone(),
                            source: SOURCE.into(),
                            project: project.clone(),
                            timestamp: ts,
                            role: "assistant".into(),
                        });
                    }
                },
            );
        }
    }

    if events.iter().all(|e| e.session_id != session.id) {
        if let Some(created) = summary.get("created_at").and_then(parse_ts) {
            events.push(TimingEvent {
                session_id: session.id.clone(),
                source: SOURCE.into(),
                project: project.clone(),
                timestamp: created,
                role: "user".into(),
            });
        }
        if let Some(updated) = summary
            .get("updated_at")
            .or_else(|| summary.get("last_active_at"))
            .and_then(parse_ts)
        {
            events.push(TimingEvent {
                session_id: session.id.clone(),
                source: SOURCE.into(),
                project,
                timestamp: updated,
                role: "assistant".into(),
            });
        }
    }

    cache::save(
        &cache_file,
        &FileCache {
            sig,
            entries: entries.clone(),
            events: events.clone(),
        },
    );
    Ok((entries, events))
}

fn emit_turn_usage(
    entries: &mut Vec<UsageEntry>,
    usage: Option<&Value>,
    project: &str,
    ts: chrono::DateTime<chrono::Utc>,
    fallback_model: &str,
) {
    let Some(usage) = usage else { return };
    if let Some(model_usage) = usage.get("modelUsage").and_then(|v| v.as_object()) {
        if !model_usage.is_empty() {
            for (model, m) in model_usage {
                push_usage(entries, model, project, ts, m);
            }
            return;
        }
    }
    push_usage(entries, fallback_model, project, ts, usage);
}

fn push_usage(
    entries: &mut Vec<UsageEntry>,
    model: &str,
    project: &str,
    ts: chrono::DateTime<chrono::Utc>,
    usage: &Value,
) {
    let total_input = to_count_opt(usage.get("inputTokens"));
    let cached = to_count_opt(usage.get("cachedReadTokens"));
    let output = to_count_opt(usage.get("outputTokens"));
    let reasoning = to_count_opt(usage.get("reasoningTokens"));
    let cache_write = to_count_opt(usage.get("cacheCreationTokens"));
    let input = (total_input - cached).max(0);
    let output = (output - reasoning).max(0);
    if input + output + cached + reasoning + cache_write == 0 {
        return;
    }
    entries.push(UsageEntry {
        source: SOURCE.into(),
        model: if model.is_empty() {
            "unknown".into()
        } else {
            model.to_string()
        },
        project: project.to_string(),
        timestamp: ts,
        input_tokens: input,
        output_tokens: output,
        cache_read_input_tokens: cached,
        cache_creation_input_tokens: cache_write,
        reasoning_output_tokens: reasoning,
    });
}
