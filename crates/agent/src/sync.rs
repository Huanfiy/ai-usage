use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use ai_usage_parsers::{default_adapters, parse_adapters, parse_all, AdapterEnv, ParseCtx};
use ai_usage_protocol::{
    CursorAccountUsage, IngestRequest, IngestResponse, UsageBucket, UsageSession, BUCKET_BATCH,
    SCHEMA_VERSION, SESSION_BATCH, SOURCE_CURSOR,
};
use anyhow::Result;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::Serialize;

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

/// One destination push within a round, with its own full/incremental mode.
#[derive(Debug, Clone)]
pub struct DestJob {
    pub dest: Destination,
    pub full: bool,
}

/// Push failure classification. Drives the scheduler:
/// Auth blocks the destination until config changes, Retryable triggers
/// backoff without advancing the clock face, Fatal is recorded and waits
/// for the next tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PushErrorKind {
    Auth,
    Fatal,
    Retryable,
}

#[derive(Debug, Clone)]
struct PushFailure {
    kind: PushErrorKind,
    msg: String,
}

impl PushFailure {
    fn retryable(msg: impl Into<String>) -> Self {
        Self {
            kind: PushErrorKind::Retryable,
            msg: msg.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceReport {
    pub source: String,
    pub buckets: usize,
    pub sessions: usize,
    pub skipped: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DestReport {
    pub url: String,
    pub full: bool,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<PushErrorKind>,
    pub ingested: u64,
    pub sessions: u64,
    pub changed_buckets: usize,
    pub changed_sessions: usize,
    pub protected: u64,
    pub dropped: u64,
}

impl DestReport {
    fn failed(url: &str, full: bool, failure: PushFailure) -> Self {
        Self {
            url: url.to_string(),
            full,
            ok: false,
            error: Some(failure.msg),
            error_kind: Some(failure.kind),
            ingested: 0,
            sessions: 0,
            changed_buckets: 0,
            changed_sessions: 0,
            protected: 0,
            dropped: 0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncReport {
    pub sources: Vec<SourceReport>,
    pub dests: Vec<DestReport>,
}

impl SyncReport {
    pub fn ingested(&self) -> u64 {
        self.dests.iter().map(|d| d.ingested).sum()
    }

    pub fn sessions_total(&self) -> u64 {
        self.dests.iter().map(|d| d.sessions).sum()
    }

    pub fn protected(&self) -> u64 {
        self.dests.iter().map(|d| d.protected).sum()
    }

    pub fn changed(&self) -> usize {
        self.dests
            .iter()
            .map(|d| d.changed_buckets + d.changed_sessions)
            .sum()
    }

    pub fn warnings(&self) -> impl Iterator<Item = &String> {
        self.sources.iter().flat_map(|s| s.warnings.iter())
    }

    pub fn parser_lines(&self) -> Vec<String> {
        self.sources
            .iter()
            .filter(|s| !s.skipped && s.buckets + s.sessions > 0)
            .map(|s| {
                format!(
                    "{:<14} {} buckets · {} sessions",
                    s.source, s.buckets, s.sessions
                )
            })
            .collect()
    }

    pub fn dest_errors(&self) -> Vec<String> {
        self.dests
            .iter()
            .filter_map(|d| d.error.as_ref().map(|e| format!("{}: {e}", d.url)))
            .collect()
    }

    pub fn all_ok(&self) -> bool {
        self.dests.iter().all(|d| d.ok)
    }

    /// Any destination failed in a way worth retrying before the next tick.
    pub fn retryable_failure(&self) -> bool {
        self.dests
            .iter()
            .any(|d| d.error_kind == Some(PushErrorKind::Retryable))
    }

    pub fn source(&self, id: &str) -> Option<&SourceReport> {
        self.sources.iter().find(|s| s.source == id)
    }
}

pub fn all_dest_jobs(cfg: &AgentConfig) -> Vec<DestJob> {
    cfg.destinations()
        .into_iter()
        .map(|dest| DestJob { dest, full: false })
        .collect()
}

pub fn dest_jobs_for_url(cfg: &AgentConfig, url: &str, full: bool) -> Result<Vec<DestJob>> {
    let key = config::normalize_url(url);
    let jobs: Vec<DestJob> = cfg
        .destinations()
        .into_iter()
        .filter(|d| d.url == key)
        .map(|dest| DestJob { dest, full })
        .collect();
    if jobs.is_empty() {
        anyhow::bail!("未配置看板地址 {key}");
    }
    Ok(jobs)
}

/// Parse once, then fan out to each destination. Per-destination failures are
/// recorded in the report instead of aborting the whole round.
pub fn run_sync_jobs(
    cfg: &AgentConfig,
    data_dir: &Path,
    quiet: bool,
    only_sources: Option<&HashSet<String>>,
    jobs: &[DestJob],
) -> Result<SyncReport> {
    if jobs.is_empty() {
        anyhow::bail!("没有配置看板地址");
    }
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

    let parsed = if let Some(only) = only_sources {
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
    let mut sources = Vec::new();
    let mut account_prune: HashMap<String, (HashSet<String>, HashSet<String>)> = HashMap::new();

    for (source, result) in parsed {
        sources.push(SourceReport {
            source: source.clone(),
            buckets: result.buckets.len(),
            sessions: result.sessions.len(),
            skipped: result.skipped,
            warnings: result.warnings.clone(),
        });
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

    // Cursor 档：为已加入账号拉套餐快照（usage-summary + Bot/Sand），
    // 归一化后随 ingest 发往各 dest。失败只记 warning，不阻塞桶上报。
    let cursor_in_scope = only_sources.map_or(true, |s| s.contains(SOURCE_CURSOR));
    let mut snapshots: Vec<CursorAccountUsage> = Vec::new();
    if cursor_in_scope && !extras.accounts.is_empty() && !cfg!(test) {
        let mut snap_warnings = Vec::new();
        for acct in &extras.accounts {
            match ai_usage_parsers::fetch_plan_with_raw(&acct.access_token) {
                Ok((snap, _raw)) => snapshots.push(account_usage(acct, snap)),
                Err(err) => snap_warnings.push(format!(
                    "Cursor: {} 套餐快照拉取失败（{}）",
                    acct.account_label,
                    plan_err_brief(err)
                )),
            }
        }
        if let Some(src) = sources.iter_mut().find(|s| s.source == SOURCE_CURSOR) {
            src.warnings.extend(snap_warnings);
        }
    }

    let primary = cfg.destinations().first().map(|d| d.url.clone());
    let mut dests = Vec::new();
    for job in jobs {
        let is_primary = primary.as_deref() == Some(job.dest.url.as_str());
        dests.push(push_dest(
            &job.dest,
            &cfg.hostname,
            data_dir,
            is_primary,
            &all_buckets,
            &all_sessions,
            &ok_sources,
            &account_prune,
            &snapshots,
            job.full,
            quiet,
        ));
    }

    let report = SyncReport { sources, dests };
    if report.all_ok() && report.changed() == 0 && !quiet {
        eprintln!("无新增数据。");
    }
    Ok(report)
}

/// 把已加入账号 + 拉到的套餐快照转成 ingest 载荷（无凭证、无原始 JSON）。
fn account_usage(
    acct: &crate::cursor_accounts::StoredAccount,
    snap: ai_usage_parsers::CursorAccountSnapshot,
) -> CursorAccountUsage {
    CursorAccountUsage {
        account_hash: acct.account_hash.clone(),
        account_label: acct.account_label.clone(),
        membership: snap.membership,
        subscription_status: snap.subscription_status,
        billing_cycle_end: snap.billing_cycle_end,
        api_percent: snap.api_percent,
        auto_percent: snap.auto_percent,
        bot_percent: snap.bot_percent,
        bot_period_start: snap.bot_period_start,
        bot_next_reset: snap.bot_next_reset,
        bot_available: snap.bot_available,
        plan_used: snap.plan_used,
        plan_limit: snap.plan_limit,
        included_cents: snap.included_cents,
        bonus_cents: snap.bonus_cents,
        auto_used: snap.auto_used,
        auto_limit: snap.auto_limit,
        fetched_at: chrono::Utc::now(),
    }
    .normalize()
}

fn plan_err_brief(err: ai_usage_parsers::PlanFetchError) -> &'static str {
    use ai_usage_parsers::PlanFetchError as E;
    match err {
        E::Token | E::Auth => "凭证失效",
        E::Network => "网络失败",
        E::Status => "接口异常",
        E::Parse => "响应无法解析",
    }
}

fn load_dest_state(
    data_dir: &Path,
    dest: &Destination,
    is_primary: bool,
) -> (SyncState, std::path::PathBuf) {
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

#[allow(clippy::too_many_arguments)]
fn push_dest(
    dest: &Destination,
    hostname: &str,
    data_dir: &Path,
    is_primary: bool,
    all_buckets: &[UsageBucket],
    all_sessions: &[UsageSession],
    ok_sources: &HashSet<String>,
    account_prune: &HashMap<String, (HashSet<String>, HashSet<String>)>,
    snapshots: &[CursorAccountUsage],
    full: bool,
    quiet: bool,
) -> DestReport {
    match push_dest_inner(
        dest,
        hostname,
        data_dir,
        is_primary,
        all_buckets,
        all_sessions,
        ok_sources,
        account_prune,
        snapshots,
        full,
        quiet,
    ) {
        Ok(report) => report,
        Err(failure) => DestReport::failed(&dest.url, full, failure),
    }
}

#[allow(clippy::too_many_arguments)]
fn push_dest_inner(
    dest: &Destination,
    hostname: &str,
    data_dir: &Path,
    is_primary: bool,
    all_buckets: &[UsageBucket],
    all_sessions: &[UsageSession],
    ok_sources: &HashSet<String>,
    account_prune: &HashMap<String, (HashSet<String>, HashSet<String>)>,
    snapshots: &[CursorAccountUsage],
    full: bool,
    quiet: bool,
) -> Result<DestReport, PushFailure> {
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
    state
        .save(&state_path)
        .map_err(|e| PushFailure::retryable(format!("写入增量 state 失败: {e:#}")))?;

    let mut report = DestReport {
        url: dest.url.clone(),
        full,
        ok: true,
        error: None,
        error_kind: None,
        ingested: 0,
        sessions: 0,
        changed_buckets: changed_buckets.len(),
        changed_sessions: changed_sessions.len(),
        protected: 0,
        dropped: 0,
    };

    let url = format!("{}/v1/ingest", dest.url);

    if changed_buckets.is_empty() && changed_sessions.is_empty() {
        // 无桶/会话变更时快照仍要送达（快照是当前状态，不参与增量 hash）
        if !snapshots.is_empty() {
            let req = IngestRequest {
                schema_version: SCHEMA_VERSION,
                hostname: Some(hostname.to_string()),
                agent_version: Some(env!("CARGO_PKG_VERSION").into()),
                timezone: Some(chrono::Local::now().format("%:z").to_string()),
                buckets: Vec::new(),
                sessions: Vec::new(),
                cursor_accounts: snapshots.to_vec(),
            };
            post_ingest(&url, &dest.token, &req)?;
        }
        return Ok(report);
    }

    if !quiet && full {
        eprintln!("全量上报 {} …", dest.url);
    }

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
            // 快照只随首批发送，避免每批重复
            cursor_accounts: if i == 0 {
                snapshots.to_vec()
            } else {
                Vec::new()
            },
        };
        let resp = post_ingest(&url, &dest.token, &req)?;
        report.ingested += resp.ingested;
        report.sessions += resp.sessions;
        report.protected += resp.protected.buckets;
        report.dropped += resp.dropped.buckets;
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
        state
            .save(&state_path)
            .map_err(|e| PushFailure::retryable(format!("写入增量 state 失败: {e:#}")))?;
    }

    Ok(report)
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

enum SendError {
    Status(u16, String),
    Transport(String),
    BadBody(String),
}

fn post_ingest(url: &str, token: &str, req: &IngestRequest) -> Result<IngestResponse, PushFailure> {
    let json = serde_json::to_vec(req)
        .map_err(|e| PushFailure::retryable(format!("序列化 ingest 请求失败: {e}")))?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&json)
        .and_then(|_| encoder.finish())
        .map_err(|e| PushFailure::retryable(format!("gzip 失败: {e}")))
        .and_then(|body| {
            let mut last: Option<PushFailure> = None;
            for attempt in 0..3 {
                match send_once(url, token, &body) {
                    Ok(resp) => return Ok(resp),
                    Err(SendError::Status(code, text)) => {
                        if code == 401 || code == 403 {
                            return Err(PushFailure {
                                kind: PushErrorKind::Auth,
                                msg: "鉴权失败：ingest token 无效或已吊销".into(),
                            });
                        }
                        // 429/408 与 5xx 都值得短退避后重试
                        if code == 429 || code == 408 || code >= 500 {
                            last = Some(PushFailure::retryable(format!("HTTP {code}: {text}")));
                        } else {
                            return Err(PushFailure {
                                kind: PushErrorKind::Fatal,
                                msg: format!("HTTP {code}: {text}"),
                            });
                        }
                    }
                    Err(SendError::Transport(msg)) => {
                        last = Some(PushFailure::retryable(msg));
                    }
                    Err(SendError::BadBody(msg)) => {
                        last = Some(PushFailure::retryable(msg));
                    }
                }
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(400 * (1 << attempt)));
                }
            }
            Err(last.unwrap_or_else(|| PushFailure::retryable("上报失败")))
        })
}

fn send_once(url: &str, token: &str, body: &[u8]) -> Result<IngestResponse, SendError> {
    let resp = ureq::post(url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .set("Content-Encoding", "gzip")
        .timeout(Duration::from_secs(60))
        .send_bytes(body);
    match resp {
        Ok(resp) => {
            let status = resp.status();
            if !(200..300).contains(&status) {
                let text = resp.into_string().unwrap_or_default();
                return Err(SendError::Status(status, text));
            }
            resp.into_json()
                .map_err(|e| SendError::BadBody(format!("解析 ingest 响应失败: {e}")))
        }
        Err(ureq::Error::Status(code, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            Err(SendError::Status(code, truncate(&text, 300)))
        }
        Err(other) => Err(SendError::Transport(other.to_string())),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;

    #[test]
    fn dest_jobs_for_url_filters() {
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
        let one = dest_jobs_for_url(&cfg, "http://10.0.0.2:3847/", true).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].dest.token, "t2");
        assert!(one[0].full);
        assert!(dest_jobs_for_url(&cfg, "http://nope:1", false).is_err());
        assert_eq!(all_dest_jobs(&cfg).len(), 2);
    }

    #[test]
    fn report_aggregates_and_classifies() {
        let report = SyncReport {
            sources: vec![
                SourceReport {
                    source: "codex".into(),
                    buckets: 2,
                    sessions: 1,
                    skipped: false,
                    warnings: vec![],
                },
                SourceReport {
                    source: "cursor".into(),
                    buckets: 0,
                    sessions: 0,
                    skipped: true,
                    warnings: vec!["登录失效".into()],
                },
            ],
            dests: vec![
                DestReport {
                    url: "http://a".into(),
                    full: false,
                    ok: true,
                    error: None,
                    error_kind: None,
                    ingested: 3,
                    sessions: 1,
                    changed_buckets: 3,
                    changed_sessions: 1,
                    protected: 1,
                    dropped: 0,
                },
                DestReport::failed(
                    "http://b",
                    false,
                    PushFailure {
                        kind: PushErrorKind::Auth,
                        msg: "鉴权失败".into(),
                    },
                ),
                DestReport::failed("http://c", false, PushFailure::retryable("超时")),
            ],
        };
        assert_eq!(report.ingested(), 3);
        assert_eq!(report.changed(), 4);
        assert!(!report.all_ok());
        assert!(report.retryable_failure());
        assert_eq!(report.dest_errors().len(), 2);
        assert!(report.source("cursor").unwrap().skipped);
        assert_eq!(report.parser_lines().len(), 1);
    }
}
