use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use ai_usage_parsers::{
    fetch_plan_with_raw, read_ide_cursor_auth, AdapterEnv, CursorAccountSnapshot,
    CursorTokenPreview, ParseCtx,
};

use crate::config::AgentConfig;
use crate::cursor_accounts::{self, AccountView, CursorAccountsFile};
use crate::sync::SyncReport;

const HTML: &str = include_str!("panel.html");
const MAX_BODY: usize = 2 * 1024 * 1024;
const MAX_HEADERS: usize = 8 * 1024;
const PLAN_TTL: Duration = Duration::from_secs(90);

pub struct PanelState {
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    inner: Mutex<Inner>,
    wake: Condvar,
    usage_cache: Mutex<HashMap<String, CachedPlan>>,
}

#[derive(Clone)]
struct CachedPlan {
    at: Instant,
    snapshot: CursorAccountSnapshot,
    raw: serde_json::Value,
}

struct Inner {
    cfg: AgentConfig,
    last_sync: Option<LastSyncView>,
    sync_requested: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LastSyncView {
    pub at: String,
    pub warnings: Vec<String>,
    pub parser_lines: Vec<String>,
    pub ingested: u64,
    pub sessions: u64,
    pub error: Option<String>,
}

impl LastSyncView {
    pub fn from_report(report: &SyncReport) -> Self {
        Self {
            at: Utc::now().to_rfc3339(),
            warnings: report.warnings.clone(),
            parser_lines: report.parser_lines.clone(),
            ingested: report.ingested,
            sessions: report.sessions,
            error: None,
        }
    }

    pub fn from_error(err: &str) -> Self {
        Self {
            at: Utc::now().to_rfc3339(),
            warnings: Vec::new(),
            parser_lines: Vec::new(),
            ingested: 0,
            sessions: 0,
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
                last_sync: None,
                sync_requested: false,
            }),
            wake: Condvar::new(),
            usage_cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> AgentConfig {
        self.inner.lock().expect("panel state").cfg.clone()
    }

    pub fn request_sync(&self) {
        let mut g = self.inner.lock().expect("panel state");
        g.sync_requested = true;
        self.wake.notify_all();
        drop(g);
        self.usage_cache.lock().expect("usage cache").clear();
    }

    fn plan_entry(&self, hash: &str, token: &str, force: bool) -> Option<CachedPlan> {
        if cfg!(test) {
            return None;
        }
        if !force {
            let cache = self.usage_cache.lock().expect("usage cache");
            if let Some(cached) = cache.get(hash) {
                if cached.at.elapsed() < PLAN_TTL {
                    return Some(cached.clone());
                }
            }
        }
        match fetch_plan_with_raw(token) {
            Some((snapshot, raw)) => {
                let entry = CachedPlan {
                    at: Instant::now(),
                    snapshot,
                    raw,
                };
                self.usage_cache
                    .lock()
                    .expect("usage cache")
                    .insert(hash.to_string(), entry.clone());
                Some(entry)
            }
            None => self
                .usage_cache
                .lock()
                .expect("usage cache")
                .get(hash)
                .cloned(),
        }
    }

    pub fn take_sync_request(&self) -> bool {
        let mut g = self.inner.lock().expect("panel state");
        let v = g.sync_requested;
        g.sync_requested = false;
        v
    }

    pub fn wait_timeout(&self, timeout: Duration) {
        let g = self.inner.lock().expect("panel state");
        if g.sync_requested {
            return;
        }
        let _ = self.wake.wait_timeout(g, timeout);
    }

    pub fn set_last_sync(&self, view: LastSyncView) {
        self.inner.lock().expect("panel state").last_sync = Some(view);
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
        ("GET", "/v1/status") => json_ok(status_payload(state)),
        ("PUT", "/v1/config") => match put_config(state, body) {
            Ok(()) => json_ok(serde_json::json!({"ok": true})),
            Err(err) => json_err(400, &err.to_string()),
        },
        ("POST", "/v1/sync") => {
            state.request_sync();
            json_status(202, serde_json::json!({"ok": true}))
        }
        ("POST", "/v1/cursor/accounts") => match post_account(state, body) {
            Ok(v) => json_ok(v),
            Err(err) => json_err(400, &err.to_string()),
        },
        (m, p) if m == "POST" && refresh_account_hash(p).is_some() => {
            let hash = refresh_account_hash(p).expect("matched");
            match refresh_account_plan(state, &hash) {
                Ok(()) => json_ok(serde_json::json!({"ok": true})),
                Err(err) => json_err(404, &err.to_string()),
            }
        }
        (m, p) if m == "DELETE" && p.starts_with("/v1/cursor/accounts/") => {
            let hash = p.trim_start_matches("/v1/cursor/accounts/");
            let hash = percent_decode(hash);
            match cursor_accounts::remove(&state.data_dir, &hash) {
                Ok(true) => {
                    state.usage_cache.lock().expect("usage cache").remove(&hash);
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
struct ConfigPatch {
    url: String,
    hostname: String,
    upload_project: bool,
    interval_local: String,
    interval_cursor: String,
}

fn put_config(state: &PanelState, body: &[u8]) -> Result<()> {
    let patch: ConfigPatch = serde_json::from_slice(body).context("JSON")?;
    let mut cfg = state.config();
    cfg.url = patch.url.trim_end_matches('/').to_string();
    cfg.hostname = patch.hostname;
    cfg.upload_project = patch.upload_project;
    cfg.interval_local = patch.interval_local;
    cfg.interval_cursor = patch.interval_cursor;
    cfg.save(&state.config_path)?;
    state.inner.lock().expect("panel state").cfg = cfg;
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
struct StatusPayload {
    url: String,
    hostname: String,
    upload_project: bool,
    interval_local: String,
    interval_cursor: String,
    bind: String,
    panel: String,
    token_prefix: String,
    tools: Vec<ToolView>,
    cursor_accounts: Vec<AccountView>,
    last_sync: Option<LastSyncView>,
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
    let last_sync = state.inner.lock().expect("panel state").last_sync.clone();
    let payload = StatusPayload {
        url: cfg.url.clone(),
        hostname: cfg.hostname.clone(),
        upload_project: cfg.upload_project,
        interval_local: cfg.interval_local.clone(),
        interval_cursor: cfg.interval_cursor.clone(),
        bind: cfg.bind.clone(),
        panel: format!("http://{}", cfg.bind),
        token_prefix: format!("{}…", cfg.token.chars().take(12).collect::<String>()),
        tools,
        cursor_accounts: {
            let mut views = merge_account_views(&extras, ide.as_ref());
            apply_live_plan(state, &mut views, &extras, ide.as_ref());
            views
        },
        last_sync,
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
    state.usage_cache.lock().expect("usage cache").remove(hash);
    let _ = state.plan_entry(hash, &token, true);
    Ok(())
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

fn apply_live_plan(
    state: &PanelState,
    views: &mut [AccountView],
    extras: &CursorAccountsFile,
    ide: Option<&CursorTokenPreview>,
) {
    if cfg!(test) || views.is_empty() {
        return;
    }
    let mut tokens: Vec<(String, String)> = Vec::new();
    if let Some(ide) = ide {
        tokens.push((ide.account_hash.clone(), ide.access_token.clone()));
    }
    for a in &extras.accounts {
        if !tokens.iter().any(|(h, _)| h == &a.account_hash) {
            tokens.push((a.account_hash.clone(), a.access_token.clone()));
        }
    }
    let snaps: Vec<(String, Option<CachedPlan>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = tokens
            .iter()
            .map(|(hash, token)| {
                scope.spawn(|| (hash.clone(), state.plan_entry(hash, token, false)))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("plan fetch"))
            .collect()
    });
    for (hash, entry) in snaps {
        if let Some(view) = views.iter_mut().find(|a| a.account_hash == hash) {
            if let Some(entry) = entry {
                view.snapshot = entry.snapshot;
                view.usage_raw = Some(entry.raw);
            }
        }
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
        assert!(state.take_sync_request());
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
}
