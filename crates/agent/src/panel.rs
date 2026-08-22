use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use ai_usage_parsers::{read_ide_cursor_auth, AdapterEnv, ParseCtx};

use crate::config::AgentConfig;
use crate::cursor_accounts::{self, AccountView};
use crate::sync::SyncReport;

const HTML: &str = include_str!("panel.html");
const MAX_BODY: usize = 64 * 1024;
const MAX_HEADERS: usize = 8 * 1024;

pub struct PanelState {
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    inner: Mutex<Inner>,
    wake: Condvar,
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
        })
    }

    pub fn config(&self) -> AgentConfig {
        self.inner.lock().expect("panel state").cfg.clone()
    }

    pub fn request_sync(&self) {
        let mut g = self.inner.lock().expect("panel state");
        g.sync_requested = true;
        self.wake.notify_all();
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
            Ok(()) => json_ok(serde_json::json!({"ok": true})),
            Err(err) => json_err(400, &err.to_string()),
        },
        (m, p) if m == "DELETE" && p.starts_with("/v1/cursor/accounts/") => {
            let hash = p.trim_start_matches("/v1/cursor/accounts/");
            let hash = percent_decode(hash);
            match cursor_accounts::remove(&state.data_dir, &hash) {
                Ok(true) => json_ok(serde_json::json!({"ok": true})),
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
    token: String,
    #[serde(default)]
    import_ide: bool,
}

fn post_account(state: &PanelState, body: &[u8]) -> Result<()> {
    let req: AddAccount = serde_json::from_slice(body).context("JSON")?;
    if req.import_ide {
        let preview = read_ide_cursor_auth(&parse_ctx(&state.data_dir))
            .ok_or_else(|| anyhow::anyhow!("未检测到 Cursor IDE 登录"))?;
        cursor_accounts::upsert(&state.data_dir, &preview)?;
        return Ok(());
    }
    cursor_accounts::add_from_raw(&state.data_dir, &req.token)?;
    Ok(())
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
    ide_cursor: Option<AccountView>,
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
    let ide = read_ide_cursor_auth(&ctx).map(|p| AccountView {
        account_hash: p.account_hash,
        account_label: p.account_label,
        exp: p.exp,
    });
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
        cursor_accounts: cursor_accounts::public_views(&extras),
        ide_cursor: ide,
        last_sync,
    };
    serde_json::to_value(payload).unwrap_or(serde_json::json!({}))
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
    fn sync_sets_flag() {
        let (_dir, state) = setup();
        let (st, _, _) = dispatch(&state, "POST", "/v1/sync", b"{}");
        assert_eq!(st, 202);
        assert!(state.take_sync_request());
    }
}
