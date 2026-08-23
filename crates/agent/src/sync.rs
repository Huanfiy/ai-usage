use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use ai_usage_parsers::{default_adapters, parse_adapters, parse_all, AdapterEnv, ParseCtx};
use ai_usage_protocol::{
    IngestRequest, IngestResponse, UsageBucket, UsageSession, BUCKET_BATCH, SCHEMA_VERSION,
    SESSION_BATCH,
};
use anyhow::{Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::config::{self, AgentConfig, Destination};
use crate::state::SyncState;

const PARSER_CONCURRENCY: usize = 4;

pub fn local_source_ids() -> [&'static str; 3] {
    [
        ai_usage_protocol::SOURCE_CLAUDE_CODE,
        ai_usage_protocol::SOURCE_CODEX,
        ai_usage_protocol::SOURCE_GROK,
    ]
}

#[derive(Clone, Copy)]
pub struct SyncOpts<'a> {
    pub only_sources: Option<&'a HashSet<String>>,
    pub only_url: Option<&'a str>,
    pub full: bool,
}

impl<'a> SyncOpts<'a> {
    pub fn all() -> Self {
        Self {
            only_sources: None,
            only_url: None,
            full: false,
        }
    }

    pub fn sources(only: &'a HashSet<String>) -> Self {
        Self {
            only_sources: Some(only),
            only_url: None,
            full: false,
        }
    }
}

pub struct SyncReport {
    pub ingested: u64,
    pub sessions: u64,
    pub changed_buckets: usize,
    pub changed_sessions: usize,
    pub parser_lines: Vec<String>,
    pub warnings: Vec<String>,
    pub protected: u64,
}

pub fn run_sync(cfg: &AgentConfig, data_dir: &Path, quiet: bool) -> Result<SyncReport> {
    run_sync_filtered(cfg, data_dir, quiet, SyncOpts::all())
}

pub fn run_sync_filtered(
    cfg: &AgentConfig,
    data_dir: &Path,
    quiet: bool,
    opts: SyncOpts<'_>,
) -> Result<SyncReport> {
    let cache_dir = data_dir.join("cache");
    std::fs::create_dir_all(&cache_dir)?;
    let home = crate::xdg::home_dir();
    let extras = crate::cursor_accounts::load(data_dir).unwrap_or_default();
    let ctx = ParseCtx {
        home,
        cache_dir,
        env: AdapterEnv {
            extra_codex_home: None,
            cursor_extra_accounts: crate::cursor_accounts::to_env(&extras),
            ..AdapterEnv::default()
        },
    };

    let parsed = if let Some(only) = opts.only_sources {
        let adapters: Vec<_> = default_adapters()
            .into_iter()
            .filter(|a| only.contains(a.id()))
            .collect();
        parse_adapters(&ctx, &adapters, PARSER_CONCURRENCY)
    } else {
        parse_all(&ctx, PARSER_CONCURRENCY)
    };
    let mut all_buckets = Vec::new();
    let mut all_sessions = Vec::new();
    let mut ok_sources = HashSet::new();
    let mut parser_lines = Vec::new();
    let mut warnings = Vec::new();
    let mut account_prune: HashMap<String, (HashSet<String>, HashSet<String>)> = HashMap::new();

    for (source, result) in parsed {
        warnings.extend(result.warnings);
        if result.skipped {
            continue;
        }
        ok_sources.insert(source.clone());
        if !result.attempted_account_hashes.is_empty() {
            account_prune.insert(
                source.clone(),
                (
                    result.attempted_account_hashes.into_iter().collect(),
                    result.succeeded_account_hashes.into_iter().collect(),
                ),
            );
        }
        if result.buckets.is_empty() && result.sessions.is_empty() {
            continue;
        }
        parser_lines.push(format!(
            "{source:<14} {} buckets · {} sessions",
            result.buckets.len(),
            result.sessions.len()
        ));
        all_buckets.extend(result.buckets);
        all_sessions.extend(result.sessions);
    }

    if !cfg.upload_project {
        for b in &mut all_buckets {
            b.project = "unknown".into();
        }
        for s in &mut all_sessions {
            s.project = "unknown".into();
        }
        all_buckets = reaggregate(all_buckets);
    }

    let dests = select_dests(cfg, opts.only_url)?;
    let primary = cfg.destinations().first().map(|d| d.url.clone());
    let mut ingested = 0u64;
    let mut sessions_synced = 0u64;
    let mut changed_buckets = 0usize;
    let mut changed_sessions = 0usize;
    let mut protected = 0u64;
    let mut errors = Vec::new();

    for dest in &dests {
        let is_primary = primary.as_deref() == Some(dest.url.as_str());
        match push_dest(
            dest,
            &cfg.hostname,
            data_dir,
            is_primary,
            &all_buckets,
            &all_sessions,
            &ok_sources,
            &account_prune,
            opts.full,
            quiet,
        ) {
            Ok(part) => {
                ingested += part.ingested;
                sessions_synced += part.sessions;
                changed_buckets += part.changed_buckets;
                changed_sessions += part.changed_sessions;
                protected += part.protected;
            }
            Err(err) => errors.push(format!("{}: {err:#}", dest.url)),
        }
    }

    if !errors.is_empty() {
        anyhow::bail!("{}", errors.join("; "));
    }
    if changed_buckets + changed_sessions == 0 && !quiet {
        eprintln!("无新增数据。");
    }

    Ok(SyncReport {
        ingested,
        sessions: sessions_synced,
        changed_buckets,
        changed_sessions,
        parser_lines,
        warnings,
        protected,
    })
}

struct DestPush {
    ingested: u64,
    sessions: u64,
    changed_buckets: usize,
    changed_sessions: usize,
    protected: u64,
}

fn select_dests(cfg: &AgentConfig, only_url: Option<&str>) -> Result<Vec<Destination>> {
    let dests = cfg.destinations();
    if dests.is_empty() {
        anyhow::bail!("没有配置看板地址");
    }
    if let Some(raw) = only_url {
        let key = config::normalize_url(raw);
        let found: Vec<_> = dests.into_iter().filter(|d| d.url == key).collect();
        if found.is_empty() {
            anyhow::bail!("未配置看板地址 {key}");
        }
        return Ok(found);
    }
    Ok(dests)
}

fn load_dest_state(data_dir: &Path, dest: &Destination, is_primary: bool) -> (SyncState, std::path::PathBuf) {
    let path = config::dest_state_path(data_dir, &dest.url);
    if path.exists() {
        return (SyncState::load(&path), path);
    }
    let legacy = data_dir.join("state.json");
    if is_primary && legacy.exists() {
        return (SyncState::load(&legacy), path);
    }
    (SyncState::default(), path)
}

fn push_dest(
    dest: &Destination,
    hostname: &str,
    data_dir: &Path,
    is_primary: bool,
    all_buckets: &[UsageBucket],
    all_sessions: &[UsageSession],
    ok_sources: &HashSet<String>,
    account_prune: &HashMap<String, (HashSet<String>, HashSet<String>)>,
    full: bool,
    quiet: bool,
) -> Result<DestPush> {
    let (mut state, state_path) = load_dest_state(data_dir, dest, is_primary);
    let mut changed_buckets = Vec::new();
    let mut changed_sessions = Vec::new();
    let mut pending_buckets = HashMap::new();
    let mut pending_sessions = HashMap::new();
    let mut live_buckets = HashSet::new();
    let mut live_sessions = HashSet::new();

    for b in all_buckets {
        let b = b.clone().normalize();
        let key = b.client_key();
        let hash = b.content_hash();
        live_buckets.insert(key.clone());
        let same = state.buckets.get(&key).map(|s| s.as_str()) == Some(hash.as_str());
        if !full && same {
            continue;
        }
        pending_buckets.insert(key, hash);
        changed_buckets.push(b);
    }
    for s in all_sessions {
        let s = s.clone().normalize();
        let key = s.client_key();
        let hash = s.content_hash();
        live_sessions.insert(key.clone());
        let same = state.sessions.get(&key).map(|h| h.as_str()) == Some(hash.as_str());
        if !full && same {
            continue;
        }
        pending_sessions.insert(key, hash);
        changed_sessions.push(s);
    }

    state.prune(&live_buckets, &live_sessions, ok_sources, account_prune);
    state.save(&state_path)?;

    if changed_buckets.is_empty() && changed_sessions.is_empty() {
        return Ok(DestPush {
            ingested: 0,
            sessions: 0,
            changed_buckets: 0,
            changed_sessions: 0,
            protected: 0,
        });
    }

    if !quiet && full {
        eprintln!("全量上报 {} …", dest.url);
    }

    let url = format!("{}/v1/ingest", dest.url);
    let mut ingested = 0u64;
    let mut sessions_synced = 0u64;
    let mut protected = 0u64;
    let bucket_batches = changed_buckets.len().div_ceil(BUCKET_BATCH);
    let session_batches = changed_sessions.len().div_ceil(SESSION_BATCH);
    let total_batches = bucket_batches.max(session_batches).max(1);

    for i in 0..total_batches {
        let batch: Vec<UsageBucket> = changed_buckets
            .iter()
            .skip(i * BUCKET_BATCH)
            .take(BUCKET_BATCH)
            .cloned()
            .collect();
        let sess: Vec<UsageSession> = changed_sessions
            .iter()
            .skip(i * SESSION_BATCH)
            .take(SESSION_BATCH)
            .cloned()
            .collect();
        let req = IngestRequest {
            schema_version: SCHEMA_VERSION,
            hostname: Some(hostname.to_string()),
            agent_version: Some(env!("CARGO_PKG_VERSION").into()),
            timezone: Some(chrono::Local::now().format("%:z").to_string()),
            buckets: batch.clone(),
            sessions: sess.clone(),
        };
        let resp = post_ingest(&url, &dest.token, &req)?;
        ingested += resp.ingested;
        sessions_synced += resp.sessions;
        protected += resp.protected.buckets;
        let unknown: HashSet<String> = resp.dropped.unknown_sources.into_iter().collect();
        for b in &batch {
            if unknown.contains(&b.source) {
                continue;
            }
            if let Some(hash) = pending_buckets.get(&b.client_key()) {
                state.buckets.insert(b.client_key(), hash.clone());
            }
        }
        for s in &sess {
            if let Some(hash) = pending_sessions.get(&s.client_key()) {
                state.sessions.insert(s.client_key(), hash.clone());
            }
        }
        state.save(&state_path)?;
    }

    Ok(DestPush {
        ingested,
        sessions: sessions_synced,
        changed_buckets: changed_buckets.len(),
        changed_sessions: changed_sessions.len(),
        protected,
    })
}

fn reaggregate(buckets: Vec<UsageBucket>) -> Vec<UsageBucket> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, UsageBucket> = BTreeMap::new();
    for b in buckets {
        let b = b.normalize();
        let key = b.client_key();
        map.entry(key)
            .and_modify(|cur| {
                cur.input_tokens += b.input_tokens;
                cur.output_tokens += b.output_tokens;
                cur.cache_read_input_tokens += b.cache_read_input_tokens;
                cur.cache_creation_input_tokens += b.cache_creation_input_tokens;
                cur.reasoning_output_tokens += b.reasoning_output_tokens;
            })
            .or_insert(b);
    }
    map.into_values().map(|b| b.normalize()).collect()
}

fn post_ingest(url: &str, token: &str, req: &IngestRequest) -> Result<IngestResponse> {
    let json = serde_json::to_vec(req)?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&json)?;
    let body = encoder.finish()?;
    let mut last_err = None;
    for attempt in 0..3 {
        match send_once(url, token, &body) {
            Ok(resp) => return Ok(resp),
            Err(err) => {
                let msg = err.to_string();
                if msg.contains("401") || msg.contains("UNAUTHORIZED") {
                    anyhow::bail!("鉴权失败：ingest token 无效");
                }
                if msg.contains("HTTP 4") {
                    return Err(err);
                }
                last_err = Some(err);
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(400 * (1 << attempt)));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("上报失败")))
}

fn send_once(url: &str, token: &str, body: &[u8]) -> Result<IngestResponse> {
    let resp = ureq::post(url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .set("Content-Encoding", "gzip")
        .timeout(Duration::from_secs(60))
        .send_bytes(body)
        .map_err(http_err)?;
    if resp.status() == 401 {
        anyhow::bail!("UNAUTHORIZED");
    }
    if resp.status() < 200 || resp.status() >= 300 {
        let status = resp.status();
        let text = resp.into_string().unwrap_or_default();
        anyhow::bail!("HTTP {status}: {text}");
    }
    resp.into_json().context("解析 ingest 响应失败")
}

fn http_err(err: ureq::Error) -> anyhow::Error {
    match err {
        ureq::Error::Status(code, resp) => {
            let text = resp.into_string().unwrap_or_default();
            anyhow::anyhow!("HTTP {code}: {text}")
        }
        other => other.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;

    #[test]
    fn select_dests_filters_url() {
        let mut cfg = AgentConfig::new(
            "http://127.0.0.1:3847".into(),
            "t1".into(),
            "h".into(),
            true,
        );
        cfg.set_destinations(vec![
            Destination::new("http://127.0.0.1:3847", "t1"),
            Destination::new("http://10.0.0.2:3847", "t2"),
        ]);
        let one = select_dests(&cfg, Some("http://10.0.0.2:3847/")).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].token, "t2");
        assert!(select_dests(&cfg, Some("http://nope:1")).is_err());
        assert_eq!(select_dests(&cfg, None).unwrap().len(), 2);
    }
}
