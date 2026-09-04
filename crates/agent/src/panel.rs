use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use ai_usage_parsers::{
    fetch_plan_with_raw, fetch_sessions, read_ide_cursor_auth, revoke_session, AdapterEnv,
    CursorAccountSnapshot, CursorSession, CursorTokenPreview, ParseCtx, PlanFetchError,
};

use crate::config::{self, AgentConfig, Destination};
use crate::cursor_accounts::{self, AccountView, CursorAccountsFile};
use crate::sync::{PushErrorKind, SourceReport, SyncReport};

const HTML: &str = include_str!("panel.html");
const ICON_FAVICON: &[u8] = include_bytes!("panel-icons/favicon.svg");
const ICON_CLAUDE_CODE: &[u8] = include_bytes!("panel-icons/claude-code.svg");
const ICON_CODEX: &[u8] = include_bytes!("panel-icons/codex.svg");
const ICON_GROK: &[u8] = include_bytes!("panel-icons/grok.svg");
const ICON_CURSOR: &[u8] = include_bytes!("panel-icons/cursor.svg");
const MAX_BODY: usize = 2 * 1024 * 1024;
const MAX_HEADERS: usize = 8 * 1024;

pub struct PanelState {
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    inner: Mutex<Inner>,
    wake: Condvar,
    usage_cache: Mutex<HashMap<String, CachedPlan>>,
    usage_errors: Mutex<HashMap<String, String>>,
}

#[derive(Clone)]
struct CachedPlan {
    snapshot: CursorAccountSnapshot,
    raw: serde_json::Value,
}

struct Inner {
    cfg: AgentConfig,
    last_sync_agent: Option<LastSyncView>,
    last_sync_cursor: Option<LastSyncView>,
    sync_jobs: Vec<SyncJob>,
    dest_sync: HashMap<String, DestSyncView>,
    last_report: Option<ReportView>,
    syncing: bool,
    /// 401 过的目标 URL：调度轮跳过，配置变更或手动同步时清除。
    auth_blocked: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct SyncJob {
    pub url: Option<String>,
    pub full: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LastSyncView {
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 非错误的状态说明（如 Cursor 未加入采集账号）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl LastSyncView {
    pub fn now_ok() -> Self {
        Self {
            at: Utc::now().to_rfc3339(),
            error: None,
            note: None,
        }
    }

    pub fn from_error(err: &str) -> Self {
        Self {
            at: Utc::now().to_rfc3339(),
            error: Some(err.to_string()),
            note: None,
        }
    }

    pub fn with_note(note: &str) -> Self {
        Self {
            at: Utc::now().to_rfc3339(),
            error: None,
            note: Some(note.to_string()),
        }
    }
}

/// 某看板地址最近一次上报结果。
#[derive(Debug, Clone, Serialize)]
pub struct DestSyncView {
    pub at: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<PushErrorKind>,
    pub full: bool,
    pub ingested: u64,
    pub sessions: u64,
    pub protected: u64,
}

/// 最近一轮同步的解析摘要。
#[derive(Debug, Clone, Serialize)]
pub struct ReportView {
    pub at: String,
    pub sources: Vec<SourceReport>,
}

impl PanelState {
    pub fn new(cfg: AgentConfig, config_path: PathBuf, data_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            config_path,
            data_dir,
            inner: Mutex::new(Inner {
                cfg,
                last_sync_agent: None,
                last_sync_cursor: None,
                sync_jobs: Vec::new(),
                dest_sync: HashMap::new(),
                last_report: None,
                syncing: false,
                auth_blocked: HashSet::new(),
            }),
            wake: Condvar::new(),
            usage_cache: Mutex::new(HashMap::new()),
            usage_errors: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> AgentConfig {
        self.inner.lock().expect("panel state").cfg.clone()
    }

    pub fn enqueue_sync(&self, job: SyncJob) {
        let mut g = self.inner.lock().expect("panel state");
        g.sync_jobs.push(job);
        self.wake.notify_all();
        drop(g);
    }

    fn cached_plan(&self, hash: &str) -> Option<CachedPlan> {
        self.usage_cache
            .lock()
            .expect("usage cache")
            .get(hash)
            .cloned()
    }

    fn plan_error(&self, hash: &str) -> Option<String> {
        self.usage_errors
            .lock()
            .expect("usage errors")
            .get(hash)
            .cloned()
    }

    fn drop_plan(&self, hash: &str) {
        self.usage_cache.lock().expect("usage cache").remove(hash);
        self.usage_errors.lock().expect("usage errors").remove(hash);
    }

    fn fetch_plan(&self, hash: &str, token: &str) -> Result<CachedPlan> {
        match fetch_plan_with_raw(token) {
            Ok((snapshot, raw)) => {
                let entry = CachedPlan { snapshot, raw };
                self.usage_cache
                    .lock()
                    .expect("usage cache")
                    .insert(hash.to_string(), entry.clone());
                self.usage_errors.lock().expect("usage errors").remove(hash);
                Ok(entry)
            }
            Err(err) => {
                let msg = plan_err_msg(err).to_string();
                self.usage_errors
                    .lock()
                    .expect("usage errors")
                    .insert(hash.to_string(), msg.clone());
                Err(anyhow::anyhow!("{msg}"))
            }
        }
    }

    #[cfg(test)]
    fn put_plan_error(&self, hash: &str, msg: &str) {
        self.usage_errors
            .lock()
            .expect("usage errors")
            .insert(hash.to_string(), msg.to_string());
    }

    pub fn take_sync_jobs(&self) -> Vec<SyncJob> {
        let mut g = self.inner.lock().expect("panel state");
        std::mem::take(&mut g.sync_jobs)
    }

    pub fn wait_timeout(&self, timeout: Duration) {
        let g = self.inner.lock().expect("panel state");
        if !g.sync_jobs.is_empty() {
            return;
        }
        let _ = self.wake.wait_timeout(g, timeout);
    }

    pub fn record_sync(&self, agent: Option<LastSyncView>, cursor: Option<LastSyncView>) {
        let mut g = self.inner.lock().expect("panel state");
        if let Some(view) = agent {
            g.last_sync_agent = Some(view);
        }
        if let Some(view) = cursor {
            g.last_sync_cursor = Some(view);
        }
    }

    pub fn set_syncing(&self, syncing: bool) {
        self.inner.lock().expect("panel state").syncing = syncing;
    }

    /// 记录整轮报告：每目标结果、解析摘要，并维护 401 封锁集合。
    pub fn record_report(&self, report: &SyncReport) {
        let now = Utc::now().to_rfc3339();
        let mut g = self.inner.lock().expect("panel state");
        for d in &report.dests {
            g.dest_sync.insert(
                d.url.clone(),
                DestSyncView {
                    at: now.clone(),
                    ok: d.ok,
                    error: d.error.clone(),
                    error_kind: d.error_kind,
                    full: d.full,
                    ingested: d.ingested,
                    sessions: d.sessions,
                    protected: d.protected,
                },
            );
            if d.error_kind == Some(PushErrorKind::Auth) {
                g.auth_blocked.insert(d.url.clone());
            } else if d.ok {
                g.auth_blocked.remove(&d.url);
            }
        }
        g.last_report = Some(ReportView {
            at: now,
            sources: report.sources.clone(),
        });
    }

    pub fn auth_blocked(&self) -> HashSet<String> {
        self.inner.lock().expect("panel state").auth_blocked.clone()
    }

    fn clear_auth_blocked(&self) {
        self.inner.lock().expect("panel state").auth_blocked.clear();
    }

    fn status_snapshot(&self) -> StatusSnapshot {
        let g = self.inner.lock().expect("panel state");
        StatusSnapshot {
            last_sync_agent: g.last_sync_agent.clone(),
            last_sync_cursor: g.last_sync_cursor.clone(),
            dest_sync: g.dest_sync.clone(),
            last_report: g.last_report.clone(),
            syncing: g.syncing,
            auth_blocked: g.auth_blocked.clone(),
        }
    }
}

struct StatusSnapshot {
    last_sync_agent: Option<LastSyncView>,
    last_sync_cursor: Option<LastSyncView>,
    dest_sync: HashMap<String, DestSyncView>,
    last_report: Option<ReportView>,
    syncing: bool,
    auth_blocked: HashSet<String>,
}

pub fn serve(listener: TcpListener, state: Arc<PanelState>) -> Result<()> {
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let state = Arc::clone(&state);
                std::thread::spawn(move || {
                    if let Err(err) = handle_stream(stream, &state) {
                        eprintln!("面板请求失败: {err:#}");
                    }
                });
            }
            Err(err) => eprintln!("面板 accept: {err}"),
        }
    }
    Ok(())
}

fn handle_stream(mut stream: TcpStream, state: &PanelState) -> Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));
    let req = read_request(&mut stream)?;
    let (status, content_type, body) = dispatch(state, &req.method, &req.path, &req.body);
    write_response(&mut stream, status, content_type, &body)
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Result<Request> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    let header_end = loop {
        let n = stream.read(&mut tmp).context("读请求")?;
        if n == 0 {
            anyhow::bail!("连接关闭");
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > MAX_HEADERS + MAX_BODY {
            anyhow::bail!("请求过大");
        }
        if let Some(i) = find_double_crlf(&buf) {
            break i;
        }
        if buf.len() > MAX_HEADERS {
            anyhow::bail!("请求头过大");
        }
    };
    let header = std::str::from_utf8(&buf[..header_end]).context("请求头非 UTF-8")?;
    let mut lines = header.split("\r\n");
    let start = lines.next().unwrap_or("");
    let mut start_parts = start.split_whitespace();
    let method = start_parts.next().unwrap_or("GET").to_string();
    let path = start_parts.next().unwrap_or("/").to_string();
    let mut content_length = 0usize;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    if content_length > MAX_BODY {
        anyhow::bail!("请求体过大");
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut tmp).context("读请求体")?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
        if body.len() > MAX_BODY {
            anyhow::bail!("请求体过大");
        }
    }
    body.truncate(content_length);
    let path = path.split('?').next().unwrap_or("/").to_string();
    Ok(Request { method, path, body })
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    Ok(())
}

fn dispatch(
    state: &PanelState,
    method: &str,
    path: &str,
    body: &[u8],
) -> (u16, &'static str, Vec<u8>) {
    match (method, path) {
        ("GET", "/") | ("GET", "/index.html") => {
            (200, "text/html; charset=utf-8", HTML.as_bytes().to_vec())
        }
        ("GET", "/favicon.svg") | ("GET", "/favicon.ico") => svg_ok(ICON_FAVICON),
        ("GET", "/icons/claude-code.svg") => svg_ok(ICON_CLAUDE_CODE),
        ("GET", "/icons/codex.svg") => svg_ok(ICON_CODEX),
        ("GET", "/icons/grok.svg") => svg_ok(ICON_GROK),
        ("GET", "/icons/cursor.svg") => svg_ok(ICON_CURSOR),
        ("GET", "/v1/status") => json_ok(status_payload(state)),
        ("PUT", "/v1/config") => match put_config(state, body) {
            Ok(v) => json_ok(v),
            Err(err) => json_err(400, &err.to_string()),
        },
        ("POST", "/v1/sync") => match post_sync(state, body) {
            Ok(()) => json_status(202, serde_json::json!({"ok": true})),
            Err(err) => json_err(400, &err.to_string()),
        },
        ("POST", "/v1/cursor/accounts") => match post_account(state, body) {
            Ok(v) => json_ok(v),
            Err(err) => json_err(400, &err.to_string()),
        },
        (m, p) if m == "POST" && refresh_account_hash(p).is_some() => {
            let hash = refresh_account_hash(p).expect("matched");
            match refresh_account_plan(state, &hash) {
                Ok(()) => json_ok(serde_json::json!({"ok": true})),
                Err(err) => {
                    let msg = err.to_string();
                    if msg == "账号不存在" {
                        json_err(404, &msg)
                    } else {
                        json_err(502, &msg)
                    }
                }
            }
        }
        (m, p) if m == "POST" && since_account_hash(p).is_some() => {
            let hash = since_account_hash(p).expect("matched");
            match post_report_since(state, &hash, body) {
                Ok(true) => json_ok(serde_json::json!({"ok": true})),
                Ok(false) => json_err(404, "账号不存在"),
                Err(err) => json_err(400, &err.to_string()),
            }
        }
        (m, p) if m == "GET" && account_subpath(p, "/sessions").is_some() => {
            let hash = account_subpath(p, "/sessions").expect("matched");
            match list_account_sessions(state, &hash) {
                Ok(sessions) => json_ok(serde_json::json!({ "sessions": sessions })),
                Err(err) => session_err(err),
            }
        }
        (m, p) if m == "POST" && account_subpath(p, "/sessions/revoke").is_some() => {
            let hash = account_subpath(p, "/sessions/revoke").expect("matched");
            match post_revoke_session(state, &hash, body) {
                Ok(()) => json_ok(serde_json::json!({"ok": true})),
                Err(err) => session_err(err),
            }
        }
        (m, p) if m == "DELETE" && p.starts_with("/v1/cursor/accounts/") => {
            let hash = p.trim_start_matches("/v1/cursor/accounts/");
            let hash = percent_decode(hash);
            match cursor_accounts::remove(&state.data_dir, &hash) {
                Ok(true) => {
                    state.drop_plan(&hash);
                    json_ok(serde_json::json!({"ok": true}))
                }
                Ok(false) => json_err(404, "账号不存在"),
                Err(err) => json_err(400, &err.to_string()),
            }
        }
        _ => json_err(404, "not found"),
    }
}

fn json_ok(v: serde_json::Value) -> (u16, &'static str, Vec<u8>) {
    json_status(200, v)
}

fn svg_ok(bytes: &[u8]) -> (u16, &'static str, Vec<u8>) {
    (200, "image/svg+xml; charset=utf-8", bytes.to_vec())
}

fn json_status(status: u16, v: serde_json::Value) -> (u16, &'static str, Vec<u8>) {
    (
        status,
        "application/json; charset=utf-8",
        serde_json::to_vec(&v).unwrap_or_else(|_| b"{}".to_vec()),
    )
}

fn json_err(status: u16, msg: &str) -> (u16, &'static str, Vec<u8>) {
    json_status(status, serde_json::json!({"error": msg}))
}

#[derive(Deserialize)]
struct DestPatch {
    url: String,
    #[serde(default)]
    token: String,
}

#[derive(Deserialize)]
struct ConfigPatch {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    destinations: Option<Vec<DestPatch>>,
    hostname: String,
    upload_project: bool,
    interval_local: String,
    interval_cursor: String,
    /// 面板监听地址；改动写入配置，重启 daemon 后生效。
    #[serde(default)]
    bind: Option<String>,
}

#[derive(Deserialize)]
struct SyncPatch {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    full: bool,
}

fn merge_destinations(old: &[Destination], incoming: &[DestPatch]) -> Result<Vec<Destination>> {
    if incoming.is_empty() {
        anyhow::bail!("至少需要一个看板地址");
    }
    let mut out = Vec::new();
    for patch in incoming {
        let url = config::normalize_url(&patch.url);
        if url.is_empty() {
            anyhow::bail!("看板地址不能为空");
        }
        let token = if patch.token.trim().is_empty() {
            old.iter()
                .find(|d| d.url == url)
                .map(|d| d.token.clone())
                .ok_or_else(|| anyhow::anyhow!("新看板地址需要 ingest token"))?
        } else {
            patch.token.trim().to_string()
        };
        out.push(Destination::new(url, token));
    }
    Ok(out)
}

fn put_config(state: &PanelState, body: &[u8]) -> Result<serde_json::Value> {
    let patch: ConfigPatch = serde_json::from_slice(body).context("JSON")?;
    let mut cfg = state.config();
    let old_dests = cfg.destinations();
    let new_dests = if let Some(dests) = patch.destinations {
        merge_destinations(&old_dests, &dests)?
    } else if let Some(url) = patch.url {
        let mut dests = old_dests.clone();
        if dests.is_empty() {
            anyhow::bail!("配置不完整");
        }
        dests[0].url = config::normalize_url(&url);
        dests
    } else {
        old_dests.clone()
    };
    let added: Vec<String> = new_dests
        .iter()
        .filter(|d| !old_dests.iter().any(|o| o.url == d.url))
        .map(|d| d.url.clone())
        .collect();
    // 同 URL 换 token：dash 端 host_id 由 token 派生，等于新主机身份；
    // 增量 hash 对新身份无意义，必须全量重传，否则看板像没数据。
    let token_changed: Vec<String> = new_dests
        .iter()
        .filter(|d| {
            old_dests
                .iter()
                .any(|o| o.url == d.url && o.token != d.token)
        })
        .map(|d| d.url.clone())
        .collect();
    cfg.set_destinations(new_dests);
    cfg.hostname = patch.hostname;
    cfg.upload_project = patch.upload_project;
    cfg.interval_local = patch.interval_local;
    cfg.interval_cursor = patch.interval_cursor;
    let mut restart_required = false;
    if let Some(bind) = patch.bind {
        let bind = bind.trim().to_string();
        if !bind.is_empty() && bind != cfg.bind {
            config::require_loopback(&bind)?;
            cfg.bind = bind;
            restart_required = true;
        }
    }
    cfg.save(&state.config_path)?;
    state.inner.lock().expect("panel state").cfg = cfg;
    // 配置变了（token 可能已换），解除 401 封锁并唤醒调度循环让新配置立即生效。
    state.clear_auth_blocked();
    state.wake.notify_all();
    let mut full_urls: Vec<String> = added;
    for url in token_changed {
        if !full_urls.contains(&url) {
            full_urls.push(url);
        }
    }
    for url in full_urls {
        state.enqueue_sync(SyncJob {
            url: Some(url),
            full: true,
        });
    }
    Ok(serde_json::json!({ "ok": true, "restart_required": restart_required }))
}

fn post_sync(state: &PanelState, body: &[u8]) -> Result<()> {
    let patch: SyncPatch = if body.is_empty() {
        SyncPatch {
            url: None,
            full: false,
        }
    } else {
        serde_json::from_slice(body).context("JSON")?
    };
    let url = patch.url.as_deref().map(config::normalize_url).filter(|u| !u.is_empty());
    if let Some(ref u) = url {
        if state.config().find_dest(u).is_none() {
            anyhow::bail!("未配置看板地址 {u}");
        }
    }
    // 手动同步是明确的重试动作，解除对应目标的 401 封锁。
    {
        let mut g = state.inner.lock().expect("panel state");
        match &url {
            Some(u) => {
                g.auth_blocked.remove(u);
            }
            None => g.auth_blocked.clear(),
        }
    }
    state.enqueue_sync(SyncJob { url, full: patch.full });
    Ok(())
}

#[derive(Deserialize)]
struct AddAccount {
    #[serde(default)]
    token: serde_json::Value,
    #[serde(default)]
    import_ide: bool,
}

fn post_account(state: &PanelState, body: &[u8]) -> Result<serde_json::Value> {
    let req: AddAccount = serde_json::from_slice(body).context("JSON")?;
    if req.import_ide {
        let preview = read_ide_cursor_auth(&parse_ctx(&state.data_dir))
            .ok_or_else(|| anyhow::anyhow!("未检测到 Cursor IDE 登录"))?;
        cursor_accounts::upsert(&state.data_dir, &preview)?;
        return Ok(serde_json::json!({
            "ok": true,
            "added": 1,
            "labels": [preview.account_label],
        }));
    }
    let raw = match &req.token {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    };
    let added = cursor_accounts::add_from_raw(&state.data_dir, &raw)?;
    Ok(serde_json::json!({
        "ok": true,
        "added": added.len(),
        "labels": added.iter().map(|a| a.account_label.clone()).collect::<Vec<_>>(),
    }))
}

fn parse_ctx(data_dir: &std::path::Path) -> ParseCtx {
    let extras = cursor_accounts::load(data_dir).unwrap_or_default();
    ParseCtx {
        home: crate::xdg::home_dir(),
        cache_dir: data_dir.join("cache"),
        env: AdapterEnv {
            cursor_extra_accounts: cursor_accounts::to_env(&extras),
            ..AdapterEnv::default()
        },
    }
}

#[derive(Serialize)]
struct DestView {
    url: String,
    token_prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last: Option<DestSyncView>,
    auth_blocked: bool,
    state_buckets: usize,
    state_sessions: usize,
}

#[derive(Serialize)]
struct StatusPayload {
    url: String,
    destinations: Vec<DestView>,
    hostname: String,
    upload_project: bool,
    interval_local: String,
    interval_cursor: String,
    bind: String,
    panel: String,
    token_prefix: String,
    version: String,
    config_path: String,
    data_dir: String,
    syncing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_tick_local: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_tick_cursor: Option<String>,
    tools: Vec<ToolView>,
    cursor_accounts: Vec<AccountView>,
    last_sync_agent: Option<LastSyncView>,
    last_sync_cursor: Option<LastSyncView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_report: Option<ReportView>,
}

#[derive(Serialize)]
struct ToolView {
    id: String,
    found: bool,
    paths: Vec<String>,
}

/// 下一个钟面刻度（刻度是绝对的 Unix 对齐点，与上次同步无关）。
fn next_tick(interval: &str) -> Option<String> {
    let period = config::parse_interval(interval).ok()?;
    let at = crate::daemon::next_aligned(std::time::SystemTime::now(), period);
    let dt: chrono::DateTime<Utc> = at.into();
    Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

fn status_payload(state: &PanelState) -> serde_json::Value {
    let cfg = state.config();
    let ctx = parse_ctx(&state.data_dir);
    let tools = ai_usage_parsers::default_adapters()
        .into_iter()
        .map(|a| {
            let paths = a.detect(&ctx);
            ToolView {
                id: a.id().to_string(),
                found: !paths.is_empty(),
                paths: paths.iter().map(|p| p.display().to_string()).collect(),
            }
        })
        .collect();
    let extras = cursor_accounts::load(&state.data_dir).unwrap_or_default();
    let ide = read_ide_cursor_auth(&ctx);
    let snap = state.status_snapshot();
    let dests = cfg.destinations();
    let dest_views: Vec<DestView> = dests
        .iter()
        .map(|d| {
            let st = crate::state::SyncState::load(&config::dest_state_path(&state.data_dir, &d.url));
            DestView {
                url: d.url.clone(),
                token_prefix: format!("{}…", d.token.chars().take(12).collect::<String>()),
                last: snap.dest_sync.get(&d.url).cloned(),
                auth_blocked: snap.auth_blocked.contains(&d.url),
                state_buckets: st.buckets.len(),
                state_sessions: st.sessions.len(),
            }
        })
        .collect();
    let first = dests.first();
    let payload = StatusPayload {
        url: first.map(|d| d.url.clone()).unwrap_or_default(),
        destinations: dest_views,
        hostname: cfg.hostname.clone(),
        upload_project: cfg.upload_project,
        interval_local: cfg.interval_local.clone(),
        interval_cursor: cfg.interval_cursor.clone(),
        bind: cfg.bind.clone(),
        panel: format!("http://{}", cfg.bind),
        token_prefix: first
            .map(|d| format!("{}…", d.token.chars().take(12).collect::<String>()))
            .unwrap_or_default(),
        version: env!("CARGO_PKG_VERSION").into(),
        config_path: state.config_path.display().to_string(),
        data_dir: state.data_dir.display().to_string(),
        syncing: snap.syncing,
        next_tick_local: next_tick(&cfg.interval_local),
        next_tick_cursor: next_tick(&cfg.interval_cursor),
        tools,
        cursor_accounts: {
            let mut views = merge_account_views(&extras, ide.as_ref());
            apply_cached_plan(state, &mut views);
            views
        },
        last_sync_agent: snap.last_sync_agent,
        last_sync_cursor: snap.last_sync_cursor,
        last_report: snap.last_report,
    };
    serde_json::to_value(payload).unwrap_or(serde_json::json!({}))
}

fn merge_account_views(
    extras: &CursorAccountsFile,
    ide: Option<&CursorTokenPreview>,
) -> Vec<AccountView> {
    let mut views = cursor_accounts::public_views(extras);
    if let Some(ide) = ide {
        if let Some(existing) = views
            .iter_mut()
            .find(|a| a.account_hash == ide.account_hash)
        {
            existing.from_ide = true;
            if ide.account_label.contains('@') {
                existing.account_label = ide.account_label.clone();
            }
            existing.ide_token_differs = extras
                .accounts
                .iter()
                .find(|a| a.account_hash == ide.account_hash)
                .map(|a| a.access_token != ide.access_token)
                .unwrap_or(false);
        } else {
            views.insert(0, AccountView::from_preview(ide, true, false));
        }
    }
    views
}

fn refresh_account_hash(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/v1/cursor/accounts/")?;
    let hash = rest.strip_suffix("/refresh")?;
    if hash.is_empty() || hash.contains('/') {
        return None;
    }
    Some(percent_decode(hash))
}

fn since_account_hash(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/v1/cursor/accounts/")?;
    let hash = rest.strip_suffix("/report-since")?;
    if hash.is_empty() || hash.contains('/') {
        return None;
    }
    Some(percent_decode(hash))
}

#[derive(Deserialize)]
struct SincePatch {
    #[serde(default)]
    report_since: Option<String>,
}

/// 修改统计起始。空/缺省 = 全部历史；接受 YYYY-MM-DD（按 UTC 0 点）或 RFC3339。
fn post_report_since(state: &PanelState, hash: &str, body: &[u8]) -> Result<bool> {
    let patch: SincePatch = if body.is_empty() {
        SincePatch { report_since: None }
    } else {
        serde_json::from_slice(body).context("JSON")?
    };
    let since = match patch.report_since {
        None => None,
        Some(raw) if raw.trim().is_empty() => None,
        Some(raw) => {
            let parsed = cursor_accounts::parse_since(&raw)
                .ok_or_else(|| anyhow::anyhow!("日期格式应为 YYYY-MM-DD 或 RFC3339"))?;
            Some(parsed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        }
    };
    let found = cursor_accounts::set_report_since(&state.data_dir, hash, since)?;
    if found {
        // 起始变化影响计入范围，尽快按新 cutoff 重算增量
        state.enqueue_sync(SyncJob {
            url: None,
            full: false,
        });
    }
    Ok(found)
}

/// `/v1/cursor/accounts/{hash}{suffix}` → hash；hash 内不允许再有 `/`。
fn account_subpath(path: &str, suffix: &str) -> Option<String> {
    let rest = path.strip_prefix("/v1/cursor/accounts/")?;
    let hash = rest.strip_suffix(suffix)?;
    if hash.is_empty() || hash.contains('/') {
        return None;
    }
    Some(percent_decode(hash))
}

/// 会话接口的错误：账号不存在 404、凭证问题 401、其余上游问题 502。
#[derive(Debug)]
enum SessionError {
    NoAccount,
    BadRequest(String),
    Upstream(PlanFetchError),
}

fn session_err(err: SessionError) -> (u16, &'static str, Vec<u8>) {
    match err {
        SessionError::NoAccount => json_err(404, "账号不存在"),
        SessionError::BadRequest(msg) => json_err(400, &msg),
        SessionError::Upstream(PlanFetchError::Token | PlanFetchError::Auth) => {
            json_err(401, plan_err_msg(PlanFetchError::Auth))
        }
        SessionError::Upstream(e) => json_err(502, plan_err_msg(e)),
    }
}

fn account_token_or_404(state: &PanelState, hash: &str) -> Result<String, SessionError> {
    let extras = cursor_accounts::load(&state.data_dir).unwrap_or_default();
    let ide = read_ide_cursor_auth(&parse_ctx(&state.data_dir));
    account_token(&extras, ide.as_ref(), hash).ok_or(SessionError::NoAccount)
}

/// 账号当前全部登录会话。测试环境不出网，返回空列表。
fn list_account_sessions(
    state: &PanelState,
    hash: &str,
) -> Result<Vec<CursorSession>, SessionError> {
    let token = account_token_or_404(state, hash)?;
    if cfg!(test) {
        return Ok(Vec::new());
    }
    fetch_sessions(&token).map_err(SessionError::Upstream)
}

#[derive(Deserialize)]
struct RevokeBody {
    session_id: String,
    #[serde(rename = "type")]
    session_type: String,
}

/// 撤销一条会话。撤销采集端自己那条会让后续采集 401，前端二次确认，
/// 这里不拦——用户可能就是要下线这台机器。
fn post_revoke_session(state: &PanelState, hash: &str, body: &[u8]) -> Result<(), SessionError> {
    let token = account_token_or_404(state, hash)?;
    let req: RevokeBody = serde_json::from_slice(body)
        .map_err(|_| SessionError::BadRequest("需要 session_id 与 type".into()))?;
    let sid = req.session_id.trim();
    if sid.is_empty() || sid.len() > 128 || !sid.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(SessionError::BadRequest("session_id 无效".into()));
    }
    if ai_usage_parsers::session_type_code(&req.session_type).is_none() {
        return Err(SessionError::BadRequest(format!(
            "不支持的会话类型 {}",
            req.session_type
        )));
    }
    if cfg!(test) {
        return Ok(());
    }
    revoke_session(&token, sid, &req.session_type).map_err(SessionError::Upstream)
}

fn refresh_account_plan(state: &PanelState, hash: &str) -> Result<()> {
    let extras = cursor_accounts::load(&state.data_dir).unwrap_or_default();
    let ide = read_ide_cursor_auth(&parse_ctx(&state.data_dir));
    let token =
        account_token(&extras, ide.as_ref(), hash).ok_or_else(|| anyhow::anyhow!("账号不存在"))?;
    if cfg!(test) {
        return Ok(());
    }
    state.fetch_plan(hash, &token).map(|_| ())
}

fn account_token(
    extras: &CursorAccountsFile,
    ide: Option<&CursorTokenPreview>,
    hash: &str,
) -> Option<String> {
    if let Some(ide) = ide {
        if ide.account_hash == hash {
            return Some(ide.access_token.clone());
        }
    }
    extras
        .accounts
        .iter()
        .find(|a| a.account_hash == hash)
        .map(|a| a.access_token.clone())
}

fn apply_cached_plan(state: &PanelState, views: &mut [AccountView]) {
    let mut credits = crate::cursor_credits::load(&state.data_dir);
    for view in views {
        if let Some(entry) = state.cached_plan(&view.account_hash) {
            view.snapshot = entry.snapshot;
            view.usage_raw = Some(entry.raw);
        }
        view.usage_error = state.plan_error(&view.account_hash);
        // 信用余额来自同步周期写的缓存文件，只在真有额度时下发
        view.credits = credits
            .remove(&view.account_hash)
            .filter(crate::cursor_credits::CreditEntry::has_credit);
    }
}

fn plan_err_msg(err: PlanFetchError) -> &'static str {
    match err {
        PlanFetchError::Token | PlanFetchError::Auth => "会话失效，请重新导入",
        PlanFetchError::Network => "拉取超时或网络失败",
        PlanFetchError::Status => "Cursor 接口返回异常",
        PlanFetchError::Parse => "无法解析套餐数据",
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00"),
                16,
            )
            .unwrap_or(b'?');
            out.push(h);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;

    fn setup() -> (tempfile::TempDir, Arc<PanelState>) {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("agent.toml");
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let cfg = AgentConfig::new(
            "http://127.0.0.1:3847".into(),
            "aiu_paneltest".into(),
            "testhost".into(),
            true,
        );
        cfg.save(&cfg_path).unwrap();
        let state = PanelState::new(cfg, cfg_path, data);
        (dir, state)
    }

    #[test]
    fn status_does_not_leak_full_token() {
        let (_dir, state) = setup();
        let (_st, _ct, body) = dispatch(&state, "GET", "/v1/status", b"");
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("aiu_panelte"));
        assert!(!text.contains("aiu_paneltest"));
        assert!(!text.contains("access_token"));
    }

    #[test]
    fn put_config_updates_interval() {
        let (_dir, state) = setup();
        let body = serde_json::json!({
            "url": "http://127.0.0.1:3847",
            "hostname": "renamed",
            "upload_project": false,
            "interval_local": "1m",
            "interval_cursor": "45m"
        });
        let (st, _, out) = dispatch(
            &state,
            "PUT",
            "/v1/config",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(st, 200, "{}", String::from_utf8_lossy(&out));
        let cfg = state.config();
        assert_eq!(cfg.hostname, "renamed");
        assert_eq!(cfg.interval_local, "1m");
        assert!(!cfg.upload_project);
        assert!(state.take_sync_jobs().is_empty());
    }

    #[test]
    fn put_config_adds_dest_and_queues_full_sync() {
        let (_dir, state) = setup();
        let body = serde_json::json!({
            "destinations": [
                { "url": "http://127.0.0.1:3847" },
                { "url": "http://10.0.0.2:3847", "token": "aiu_second" }
            ],
            "hostname": "testhost",
            "upload_project": true,
            "interval_local": "5m",
            "interval_cursor": "30m"
        });
        let (st, _, out) = dispatch(
            &state,
            "PUT",
            "/v1/config",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(st, 200, "{}", String::from_utf8_lossy(&out));
        let dests = state.config().destinations();
        assert_eq!(dests.len(), 2);
        assert_eq!(dests[0].token, "aiu_paneltest");
        assert_eq!(dests[1].url, "http://10.0.0.2:3847");
        let jobs = state.take_sync_jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].url.as_deref(), Some("http://10.0.0.2:3847"));
        assert!(jobs[0].full);
    }

    #[test]
    fn put_config_token_change_queues_full_sync() {
        let (_dir, state) = setup();
        let body = serde_json::json!({
            "destinations": [
                { "url": "http://127.0.0.1:3847", "token": "aiu_rotated" }
            ],
            "hostname": "testhost",
            "upload_project": true,
            "interval_local": "5m",
            "interval_cursor": "30m"
        });
        let (st, _, out) = dispatch(
            &state,
            "PUT",
            "/v1/config",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(st, 200, "{}", String::from_utf8_lossy(&out));
        let jobs = state.take_sync_jobs();
        assert_eq!(jobs.len(), 1, "换 token = 新 host_id，须全量");
        assert_eq!(jobs[0].url.as_deref(), Some("http://127.0.0.1:3847"));
        assert!(jobs[0].full);
        // token 未变时不触发
        let body = serde_json::json!({
            "destinations": [ { "url": "http://127.0.0.1:3847" } ],
            "hostname": "testhost",
            "upload_project": true,
            "interval_local": "5m",
            "interval_cursor": "30m"
        });
        let (st, _, _) = dispatch(
            &state,
            "PUT",
            "/v1/config",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(st, 200);
        assert!(state.take_sync_jobs().is_empty());
    }

    #[test]
    fn put_config_new_dest_requires_token() {
        let (_dir, state) = setup();
        let body = serde_json::json!({
            "destinations": [
                { "url": "http://127.0.0.1:3847" },
                { "url": "http://10.0.0.2:3847" }
            ],
            "hostname": "testhost",
            "upload_project": true,
            "interval_local": "5m",
            "interval_cursor": "30m"
        });
        let (st, _, _) = dispatch(
            &state,
            "PUT",
            "/v1/config",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(st, 400);
    }

    #[test]
    fn add_account_rejects_garbage() {
        let (_dir, state) = setup();
        let body = serde_json::json!({"token": "nope"});
        let (st, _, _) = dispatch(
            &state,
            "POST",
            "/v1/cursor/accounts",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(st, 400);
    }

    #[test]
    fn add_accounts_from_json_dump() {
        let (_dir, state) = setup();
        let jwt = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1c2VyX3QiLCJlbWFpbCI6InRAZS5jb20ifQ.sig";
        let dump = serde_json::json!([{
            "email": "t@e.com",
            "access_token": jwt,
            "membership_type": "pro",
            "cursor_usage_raw": {
                "autoModelSelectedDisplayMessage": "You've used 14% of your included total usage",
                "individualUsage": { "plan": { "used": 200, "limit": 2000, "totalPercentUsed": 14.0 } }
            }
        }]);
        let body = serde_json::json!({ "token": dump.to_string() });
        let (st, _, out) = dispatch(
            &state,
            "POST",
            "/v1/cursor/accounts",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(st, 200, "{}", String::from_utf8_lossy(&out));
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["added"], 1);
        let (_st, _, status) = dispatch(&state, "GET", "/v1/status", b"");
        let status: serde_json::Value = serde_json::from_slice(&status).unwrap();
        let accts = status["cursor_accounts"].as_array().unwrap();
        let acct = accts
            .iter()
            .find(|a| a["account_label"] == "t@e.com")
            .expect("imported dump account");
        assert_eq!(acct["stored"], true);
        assert!(acct.get("membership").is_none() || acct["membership"].is_null());
        assert!(!status.to_string().contains("access_token"));
    }

    #[test]
    fn sync_sets_flag() {
        let (_dir, state) = setup();
        let (st, _, _) = dispatch(&state, "POST", "/v1/sync", b"{}");
        assert_eq!(st, 202);
        let jobs = state.take_sync_jobs();
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].url.is_none());
        assert!(!jobs[0].full);
    }

    #[test]
    fn sync_one_url_incremental() {
        let (_dir, state) = setup();
        let body = serde_json::json!({ "url": "http://127.0.0.1:3847/" });
        let (st, _, _) = dispatch(
            &state,
            "POST",
            "/v1/sync",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(st, 202);
        let jobs = state.take_sync_jobs();
        assert_eq!(jobs[0].url.as_deref(), Some("http://127.0.0.1:3847"));
        assert!(!jobs[0].full);
    }

    #[test]
    fn sync_unknown_url_is_400() {
        let (_dir, state) = setup();
        let body = serde_json::json!({ "url": "http://nope:1" });
        let (st, _, _) = dispatch(
            &state,
            "POST",
            "/v1/sync",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(st, 400);
    }

    #[test]
    fn serves_official_tool_icons() {
        let (_dir, state) = setup();
        for name in ["claude-code", "codex", "grok", "cursor"] {
            let path = format!("/icons/{name}.svg");
            let (st, ct, body) = dispatch(&state, "GET", &path, b"");
            assert_eq!(st, 200, "{path}");
            assert!(ct.starts_with("image/svg+xml"));
            let text = String::from_utf8(body).unwrap();
            assert!(text.contains("<svg"), "{path}");
        }
        let (st, _, _) = dispatch(&state, "GET", "/icons/nope.svg", b"");
        assert_eq!(st, 404);
    }

    #[test]
    fn serves_favicon() {
        let (_dir, state) = setup();
        let (st, _, html) = dispatch(&state, "GET", "/", b"");
        assert_eq!(st, 200);
        assert!(String::from_utf8(html).unwrap().contains("/favicon.svg"));
        for path in ["/favicon.svg", "/favicon.ico"] {
            let (st, ct, body) = dispatch(&state, "GET", path, b"");
            assert_eq!(st, 200, "{path}");
            assert!(ct.starts_with("image/svg+xml"));
            let text = String::from_utf8(body).unwrap();
            assert!(text.contains("<svg"), "{path}");
            assert!(text.contains("AI Usage Agent"), "{path}");
        }
    }

    #[test]
    fn record_sync_splits_agent_and_cursor() {
        let (_dir, state) = setup();
        state.record_sync(Some(LastSyncView::now_ok()), None);
        let (_st, _, body) = dispatch(&state, "GET", "/v1/status", b"");
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["last_sync_agent"].is_object());
        assert!(v["last_sync_agent"]["error"].is_null());
        assert!(v["last_sync_cursor"].is_null());

        state.record_sync(None, Some(LastSyncView::from_error("cursor down")));
        let (_st, _, body) = dispatch(&state, "GET", "/v1/status", b"");
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["last_sync_agent"].is_object());
        assert_eq!(v["last_sync_cursor"]["error"], "cursor down");
    }

    fn sample_report(ok: bool) -> SyncReport {
        use crate::sync::DestReport;
        SyncReport {
            sources: vec![SourceReport {
                source: "codex".into(),
                buckets: 2,
                sessions: 1,
                skipped: false,
                warnings: vec![],
            }],
            dests: vec![DestReport {
                url: "http://127.0.0.1:3847".into(),
                full: false,
                ok,
                error: if ok { None } else { Some("鉴权失败".into()) },
                error_kind: if ok { None } else { Some(PushErrorKind::Auth) },
                ingested: 2,
                sessions: 1,
                changed_buckets: 2,
                changed_sessions: 1,
                protected: 0,
                dropped: 0,
            }],
        }
    }

    #[test]
    fn status_exposes_dest_results_and_meta() {
        let (_dir, state) = setup();
        state.record_report(&sample_report(true));
        state.set_syncing(true);
        let (_st, _, body) = dispatch(&state, "GET", "/v1/status", b"");
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
        assert!(v["config_path"].as_str().unwrap().ends_with("agent.toml"));
        assert!(!v["data_dir"].as_str().unwrap().is_empty());
        assert_eq!(v["syncing"], true);
        assert!(v["next_tick_local"].is_string());
        assert!(v["next_tick_cursor"].is_string());
        let dest = &v["destinations"][0];
        assert_eq!(dest["last"]["ok"], true);
        assert_eq!(dest["last"]["ingested"], 2);
        assert_eq!(dest["auth_blocked"], false);
        assert!(dest["state_buckets"].is_number());
        assert_eq!(v["last_report"]["sources"][0]["source"], "codex");
        let tools = v["tools"].as_array().unwrap();
        assert!(tools.iter().all(|t| t["paths"].is_array()));
    }

    #[test]
    fn auth_failure_blocks_until_config_or_manual_sync() {
        let (_dir, state) = setup();
        state.record_report(&sample_report(false));
        assert!(state.auth_blocked().contains("http://127.0.0.1:3847"));
        let (_st, _, body) = dispatch(&state, "GET", "/v1/status", b"");
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["destinations"][0]["auth_blocked"], true);
        assert_eq!(v["destinations"][0]["last"]["error_kind"], "auth");

        // 手动同步解除封锁
        let (st, _, _) = dispatch(&state, "POST", "/v1/sync", b"{}");
        assert_eq!(st, 202);
        assert!(state.auth_blocked().is_empty());

        // 配置保存也解除封锁
        state.record_report(&sample_report(false));
        assert!(!state.auth_blocked().is_empty());
        let body = serde_json::json!({
            "url": "http://127.0.0.1:3847",
            "hostname": "testhost",
            "upload_project": true,
            "interval_local": "5m",
            "interval_cursor": "30m"
        });
        let (st, _, _) = dispatch(
            &state,
            "PUT",
            "/v1/config",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(st, 200);
        assert!(state.auth_blocked().is_empty());
    }

    #[test]
    fn put_config_bind_loopback_and_restart_flag() {
        let (_dir, state) = setup();
        let mk = |bind: &str| {
            serde_json::to_vec(&serde_json::json!({
                "url": "http://127.0.0.1:3847",
                "hostname": "testhost",
                "upload_project": true,
                "interval_local": "5m",
                "interval_cursor": "30m",
                "bind": bind
            }))
            .unwrap()
        };
        let (st, _, _) = dispatch(&state, "PUT", "/v1/config", &mk("0.0.0.0:3848"));
        assert_eq!(st, 400, "非回环 bind 必须拒绝");
        let (st, _, out) = dispatch(&state, "PUT", "/v1/config", &mk("127.0.0.1:4000"));
        assert_eq!(st, 200, "{}", String::from_utf8_lossy(&out));
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["restart_required"], true);
        assert_eq!(state.config().bind, "127.0.0.1:4000");
        // 不带变化的 bind 不要求重启
        let (st, _, out) = dispatch(&state, "PUT", "/v1/config", &mk("127.0.0.1:4000"));
        assert_eq!(st, 200);
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["restart_required"], false);
    }

    #[test]
    fn report_since_endpoint_updates_and_queues_sync() {
        let (_dir, state) = setup();
        let jwt = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1c2VyX3QiLCJlbWFpbCI6InRAZS5jb20ifQ.sig";
        let body = serde_json::json!({ "token": jwt });
        let (st, _, _) = dispatch(
            &state,
            "POST",
            "/v1/cursor/accounts",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(st, 200);
        // 本机可能存在真实 IDE 登录被扫描进列表，按邮箱定位测试账号
        let find = |v: &serde_json::Value| -> serde_json::Value {
            v["cursor_accounts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["account_label"] == "t@e.com")
                .cloned()
                .expect("test account present")
        };
        let (_st, _, status) = dispatch(&state, "GET", "/v1/status", b"");
        let v: serde_json::Value = serde_json::from_slice(&status).unwrap();
        let acct = find(&v);
        let hash = acct["account_hash"].as_str().unwrap().to_string();
        assert!(acct["added_at"].is_string(), "新账号记录加入时间");
        assert!(acct["report_since"].is_string(), "默认从加入起报");

        let path = format!("/v1/cursor/accounts/{hash}/report-since");
        let body = serde_json::json!({ "report_since": "2026-01-01" });
        let (st, _, out) = dispatch(&state, "POST", &path, &serde_json::to_vec(&body).unwrap());
        assert_eq!(st, 200, "{}", String::from_utf8_lossy(&out));
        let jobs = state.take_sync_jobs();
        assert_eq!(jobs.len(), 1, "起始变化应触发一次增量同步");
        assert!(!jobs[0].full);
        let (_st, _, status) = dispatch(&state, "GET", "/v1/status", b"");
        let v: serde_json::Value = serde_json::from_slice(&status).unwrap();
        assert_eq!(find(&v)["report_since"], "2026-01-01T00:00:00Z");

        // 清空 = 全部历史
        let (st, _, _) = dispatch(&state, "POST", &path, br#"{"report_since":null}"#);
        assert_eq!(st, 200);
        let (_st, _, status) = dispatch(&state, "GET", "/v1/status", b"");
        let v: serde_json::Value = serde_json::from_slice(&status).unwrap();
        assert!(find(&v).get("report_since").is_none());

        // 非法日期 400；未知账号 404
        let (st, _, _) = dispatch(&state, "POST", &path, br#"{"report_since":"nope"}"#);
        assert_eq!(st, 400);
        let (st, _, _) = dispatch(
            &state,
            "POST",
            "/v1/cursor/accounts/unknown/report-since",
            br#"{}"#,
        );
        assert_eq!(st, 404);
    }

    #[test]
    fn refresh_unknown_account_is_404() {
        let (_dir, state) = setup();
        let (st, _, _) = dispatch(&state, "POST", "/v1/cursor/accounts/nope/refresh", b"");
        assert_eq!(st, 404);
    }

    #[test]
    fn refresh_stored_account_ok() {
        let (_dir, state) = setup();
        let jwt = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1c2VyX3QiLCJlbWFpbCI6InRAZS5jb20ifQ.sig";
        let body = serde_json::json!({ "token": jwt });
        let (st, _, _) = dispatch(
            &state,
            "POST",
            "/v1/cursor/accounts",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(st, 200);
        let (_st, _, status) = dispatch(&state, "GET", "/v1/status", b"");
        let status: serde_json::Value = serde_json::from_slice(&status).unwrap();
        let hash = status["cursor_accounts"][0]["account_hash"]
            .as_str()
            .unwrap();
        let path = format!("/v1/cursor/accounts/{hash}/refresh");
        let (st, _, out) = dispatch(&state, "POST", &path, b"");
        assert_eq!(st, 200, "{}", String::from_utf8_lossy(&out));
    }

    #[test]
    fn sessions_routes_validate_account_and_body() {
        let (_dir, state) = setup();
        let (st, _, _) = dispatch(&state, "GET", "/v1/cursor/accounts/nope/sessions", b"");
        assert_eq!(st, 404);
        let (st, _, _) = dispatch(
            &state,
            "POST",
            "/v1/cursor/accounts/nope/sessions/revoke",
            b"{}",
        );
        assert_eq!(st, 404);

        let jwt = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1c2VyX3QiLCJlbWFpbCI6InRAZS5jb20ifQ.sig";
        let body = serde_json::json!({ "token": jwt });
        let (st, _, _) = dispatch(
            &state,
            "POST",
            "/v1/cursor/accounts",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(st, 200);
        let (_st, _, status) = dispatch(&state, "GET", "/v1/status", b"");
        let status: serde_json::Value = serde_json::from_slice(&status).unwrap();
        let hash = status["cursor_accounts"][0]["account_hash"]
            .as_str()
            .unwrap()
            .to_string();

        let (st, _, out) = dispatch(
            &state,
            "GET",
            &format!("/v1/cursor/accounts/{hash}/sessions"),
            b"",
        );
        assert_eq!(st, 200);
        let out: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert!(out["sessions"].as_array().unwrap().is_empty());

        let revoke = format!("/v1/cursor/accounts/{hash}/sessions/revoke");
        let post = |body: &serde_json::Value| {
            dispatch(&state, "POST", &revoke, &serde_json::to_vec(body).unwrap())
        };
        let (st, _, _) = dispatch(&state, "POST", &revoke, b"not json");
        assert_eq!(st, 400);
        let (st, _, _) =
            post(&serde_json::json!({ "session_id": "../x", "type": "SESSION_TYPE_WEB" }));
        assert_eq!(st, 400);
        let (st, _, _) =
            post(&serde_json::json!({ "session_id": "abc123", "type": "SESSION_TYPE_FUTURE" }));
        assert_eq!(st, 400);
        let (st, _, out) =
            post(&serde_json::json!({ "session_id": "abc123", "type": "SESSION_TYPE_CLIENT" }));
        assert_eq!(st, 200, "{}", String::from_utf8_lossy(&out));
    }

    #[test]
    fn status_exposes_credits_only_when_positive() {
        let (_dir, state) = setup();
        let jwt = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1c2VyX3QiLCJlbWFpbCI6InRAZS5jb20ifQ.sig";
        let body = serde_json::json!({ "token": jwt });
        dispatch(
            &state,
            "POST",
            "/v1/cursor/accounts",
            &serde_json::to_vec(&body).unwrap(),
        );
        let (_st, _, status) = dispatch(&state, "GET", "/v1/status", b"");
        let status: serde_json::Value = serde_json::from_slice(&status).unwrap();
        let hash = status["cursor_accounts"][0]["account_hash"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(status["cursor_accounts"][0].get("credits").is_none());

        let mk = |total: i64| crate::cursor_credits::CreditEntry {
            fetched_at: "2026-09-04T00:00:00Z".into(),
            remaining_cents: Some(total),
            total_cents: Some(total),
            expires_at: None,
            label: Some("Promo".into()),
            grants: vec![serde_json::json!({"displayName": "Promo"})],
        };
        let mut zero = std::collections::HashMap::new();
        zero.insert(hash.clone(), mk(0));
        crate::cursor_credits::store(&state.data_dir, &zero, |_| true).unwrap();
        let (_st, _, status) = dispatch(&state, "GET", "/v1/status", b"");
        let status: serde_json::Value = serde_json::from_slice(&status).unwrap();
        assert!(status["cursor_accounts"][0].get("credits").is_none());

        let mut some = std::collections::HashMap::new();
        some.insert(hash, mk(10000));
        crate::cursor_credits::store(&state.data_dir, &some, |_| true).unwrap();
        let (_st, _, status) = dispatch(&state, "GET", "/v1/status", b"");
        let status: serde_json::Value = serde_json::from_slice(&status).unwrap();
        let credits = &status["cursor_accounts"][0]["credits"];
        assert_eq!(credits["total_cents"], 10000);
        assert_eq!(credits["grants"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn status_shows_plan_error_without_fetching() {
        let (_dir, state) = setup();
        let jwt = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1c2VyX3QiLCJlbWFpbCI6InRAZS5jb20ifQ.sig";
        let body = serde_json::json!({ "token": jwt });
        let (st, _, _) = dispatch(
            &state,
            "POST",
            "/v1/cursor/accounts",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(st, 200);
        let (_st, _, status) = dispatch(&state, "GET", "/v1/status", b"");
        let status: serde_json::Value = serde_json::from_slice(&status).unwrap();
        let hash = status["cursor_accounts"][0]["account_hash"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(status["cursor_accounts"][0].get("usage_error").is_none());
        state.put_plan_error(&hash, "会话失效，请重新导入");
        let (_st, _, status) = dispatch(&state, "GET", "/v1/status", b"");
        let status: serde_json::Value = serde_json::from_slice(&status).unwrap();
        assert_eq!(
            status["cursor_accounts"][0]["usage_error"],
            "会话失效，请重新导入"
        );
    }
}
