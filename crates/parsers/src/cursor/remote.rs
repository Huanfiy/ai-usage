use std::time::Duration;

const DEFAULT_BASE: &str = "https://cursor.com";
const EXPORT_PATH: &str = "/api/dashboard/export-usage-events-csv?strategy=tokens";
const SUMMARY_PATH: &str = "/api/usage-summary";
const SESSIONS_PATH: &str = "/api/auth/sessions";
const SESSIONS_REVOKE_PATH: &str = "/api/auth/sessions/revoke";
const SESSION_COOKIE: &str = "WorkosCursorSessionToken";
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36";
const TIMEOUT_SECS: u64 = 20;
const SUMMARY_TIMEOUT_SECS: u64 = 10;

/// Bot/Sand 配额走 Cursor 私有 ConnectRPC，只认原生 access token（Bearer），
/// 网站登录的 `type=web` JWT 会被 401/403 拒绝。
const SAND_DEFAULT_BASE: &str = "https://api2.cursor.sh";
const SAND_PATH: &str = "/aiserver.v1.DashboardService/GetSandUsageStatus";
/// 随仓库版本更新的客户端版本头快照。
const SAND_CLIENT_VERSION: &str = "3.17.21";
const SAND_TIMEOUT_SECS: u64 = 10;

#[derive(Debug)]
pub enum FetchError {
    Auth,
    Network,
    Status,
}

pub fn fetch_usage_csv(sub: &str, jwt: &str) -> Result<String, FetchError> {
    fetch_with_cookies(sub, jwt, |cookie| {
        get_body(&export_url(), cookie, "text/csv,*/*;q=0.8", TIMEOUT_SECS)
    })
}

pub fn fetch_usage_summary(sub: &str, jwt: &str) -> Result<String, FetchError> {
    fetch_with_cookies(sub, jwt, |cookie| {
        get_body(
            &summary_url(),
            cookie,
            "application/json",
            SUMMARY_TIMEOUT_SECS,
        )
    })
}

/// `GET /api/auth/sessions`：账号当前全部登录会话（Web / Desktop / …）。
pub fn fetch_sessions(sub: &str, jwt: &str) -> Result<String, FetchError> {
    fetch_with_cookies(sub, jwt, |cookie| {
        get_body(
            &web_url(SESSIONS_PATH),
            cookie,
            "application/json",
            SUMMARY_TIMEOUT_SECS,
        )
    })
}

/// `POST /api/auth/sessions/revoke`。`type_code` 为服务端枚举数字
/// （WEB=1 / CLIENT=2 / MOBILE=10 / CHROME_EXTENSION=11），不是字符串。
/// 服务端异步失效，撤销后短时间内该会话可能仍可用。
pub fn revoke_session(
    sub: &str,
    jwt: &str,
    session_id: &str,
    type_code: u32,
) -> Result<String, FetchError> {
    let body = serde_json::json!({ "session_id": session_id, "type": type_code }).to_string();
    fetch_with_cookies(sub, jwt, |cookie| {
        post_json(
            &web_url(SESSIONS_REVOKE_PATH),
            cookie,
            &body,
            SUMMARY_TIMEOUT_SECS,
        )
    })
}

/// `POST GetSandUsageStatus`：Bearer + Connect 头，空对象请求体。
pub fn fetch_sand_usage(jwt: &str) -> Result<String, FetchError> {
    let resp = ureq::post(&sand_url())
        .set("Authorization", &format!("Bearer {jwt}"))
        .set("Content-Type", "application/json")
        .set("Connect-Protocol-Version", "1")
        .set("x-cursor-client-type", "sand")
        .set("x-cursor-client-version", SAND_CLIENT_VERSION)
        .timeout(Duration::from_secs(SAND_TIMEOUT_SECS))
        .send_string("{}");
    match resp {
        Ok(resp) => resp.into_string().map_err(|_| FetchError::Network),
        Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => Err(FetchError::Auth),
        Err(ureq::Error::Status(_, _)) => Err(FetchError::Status),
        Err(_) => Err(FetchError::Network),
    }
}

fn sand_url() -> String {
    let base = std::env::var("CURSOR_SAND_BASE_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| SAND_DEFAULT_BASE.to_string());
    format!("{base}{SAND_PATH}")
}

fn fetch_with_cookies(
    sub: &str,
    jwt: &str,
    mut get: impl FnMut(&str) -> Result<String, FetchError>,
) -> Result<String, FetchError> {
    let mut last_auth = false;
    for cookie in cookie_values(sub, jwt) {
        match get(&cookie) {
            Ok(body) => return Ok(body),
            Err(FetchError::Auth) => {
                last_auth = true;
                continue;
            }
            Err(other) => return Err(other),
        }
    }
    if last_auth {
        Err(FetchError::Auth)
    } else {
        Err(FetchError::Network)
    }
}

fn export_url() -> String {
    web_url(EXPORT_PATH)
}

fn summary_url() -> String {
    web_url(SUMMARY_PATH)
}

fn web_url(path: &str) -> String {
    format!("{}{path}", web_base())
}

fn web_base() -> String {
    std::env::var("CURSOR_WEB_BASE_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_string())
}

fn cookie_values(sub: &str, jwt: &str) -> Vec<String> {
    let encoded = format!("{}%3A%3A{jwt}", percent_encode(sub));
    let raw = format!("{sub}%3A%3A{jwt}");
    if encoded == raw {
        vec![encoded]
    } else {
        vec![encoded, raw]
    }
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn get_body(
    url: &str,
    cookie_value: &str,
    accept: &str,
    timeout_secs: u64,
) -> Result<String, FetchError> {
    let resp = ureq::get(url)
        .set("Cookie", &format!("{SESSION_COOKIE}={cookie_value}"))
        .set("Accept", accept)
        .set("Origin", DEFAULT_BASE)
        .set("Referer", "https://cursor.com/dashboard?tab=usage")
        .set("User-Agent", UA)
        .timeout(Duration::from_secs(timeout_secs))
        .call();
    map_response(resp)
}

fn post_json(
    url: &str,
    cookie_value: &str,
    body: &str,
    timeout_secs: u64,
) -> Result<String, FetchError> {
    let resp = ureq::post(url)
        .set("Cookie", &format!("{SESSION_COOKIE}={cookie_value}"))
        .set("Accept", "application/json")
        .set("Content-Type", "application/json")
        .set("Origin", DEFAULT_BASE)
        .set("Referer", "https://cursor.com/dashboard/settings")
        .set("User-Agent", UA)
        .timeout(Duration::from_secs(timeout_secs))
        .send_string(body);
    map_response(resp)
}

fn map_response(resp: Result<ureq::Response, ureq::Error>) -> Result<String, FetchError> {
    match resp {
        Ok(resp) => resp.into_string().map_err(|_| FetchError::Network),
        Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => Err(FetchError::Auth),
        Err(ureq::Error::Status(_, _)) => Err(FetchError::Status),
        Err(_) => Err(FetchError::Network),
    }
}
