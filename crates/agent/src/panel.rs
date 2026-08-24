use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use ai_usage_parsers::{
    fetch_plan_with_raw, read_ide_cursor_auth, AdapterEnv, CursorAccountSnapshot,
    CursorTokenPreview, ParseCtx, PlanFetchError,
};

use crate::config::{self, AgentConfig, Destination};
use crate::cursor_accounts::{self, AccountView, CursorAccountsFile};

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
}

impl LastSyncView {
    pub fn now_ok() -> Self {
        Self {
            at: Utc::now().to_rfc3339(),
            error: None,
        }
    }

    pub fn from_error(err: &str) -> Self {
        Self {
            at: Utc::now().to_rfc3339(),
            error: Some(err.to_string()),
        }
    }
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

    pub fn record_sync(&self, agent: bool, cursor: bool, view: LastSyncView) {
        let mut g = self.inner.lock().expect("panel state");
        if agent {
            g.last_sync_agent = Some(view.clone());
        }
        if cursor {
            g.last_sync_cursor = Some(view);
        }
    }
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
            Ok(()) => json_ok(serde_json::json!({"ok": true})),
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

fn put_config(state: &PanelState, body: &[u8]) -> Result<()> {
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
    cfg.set_destinations(new_dests);
    cfg.hostname = patch.hostname;
    cfg.upload_project = patch.upload_project;
    cfg.interval_local = patch.interval_local;
    cfg.interval_cursor = patch.interval_cursor;
    cfg.save(&state.config_path)?;
    state.inner.lock().expect("panel state").cfg = cfg;
    for url in added {
        state.enqueue_sync(SyncJob {
            url: Some(url),
            full: true,
        });
    }
    Ok(())
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
    tools: Vec<ToolView>,
    cursor_accounts: Vec<AccountView>,
    last_sync_agent: Option<LastSyncView>,
    last_sync_cursor: Option<LastSyncView>,
}

#[derive(Serialize)]
struct ToolView {
    id: String,
    found: bool,
}

fn status_payload(state: &PanelState) -> serde_json::Value {
    let cfg = state.config();
    let ctx = parse_ctx(&state.data_dir);
    let tools = ai_usage_parsers::default_adapters()
        .into_iter()
        .map(|a| ToolView {
            id: a.id().to_string(),
            found: !a.detect(&ctx).is_empty(),
        })
        .collect();
    let extras = cursor_accounts::load(&state.data_dir).unwrap_or_default();
    let ide = read_ide_cursor_auth(&ctx);
    let (last_sync_agent, last_sync_cursor) = {
        let g = state.inner.lock().expect("panel state");
        (g.last_sync_agent.clone(), g.last_sync_cursor.clone())
    };
    let dests = cfg.destinations();
    let dest_views: Vec<DestView> = dests
        .iter()
        .map(|d| DestView {
            url: d.url.clone(),
            token_prefix: format!("{}…", d.token.chars().take(12).collect::<String>()),
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
        tools,
        cursor_accounts: {
            let mut views = merge_account_views(&extras, ide.as_ref());
            apply_cached_plan(state, &mut views);
            views
        },
        last_sync_agent,
        last_sync_cursor,
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
    for view in views {
        if let Some(entry) = state.cached_plan(&view.account_hash) {
            view.snapshot = entry.snapshot;
            view.usage_raw = Some(entry.raw);
        }
        view.usage_error = state.plan_error(&view.account_hash);
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
        state.record_sync(true, false, LastSyncView::now_ok());
        let (_st, _, body) = dispatch(&state, "GET", "/v1/status", b"");
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["last_sync_agent"].is_object());
        assert!(v["last_sync_agent"]["error"].is_null());
        assert!(v["last_sync_cursor"].is_null());

        state.record_sync(false, true, LastSyncView::from_error("cursor down"));
        let (_st, _, body) = dispatch(&state, "GET", "/v1/status", b"");
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["last_sync_agent"].is_object());
        assert_eq!(v["last_sync_cursor"]["error"], "cursor down");
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
