//! Cursor usage is the account CSV on cursor.com, not local bubbles.
//!
//! Layers: `paths` finds `state.vscdb` → `local` reads the JWT row → `remote`
//! fetches the CSV → `csv` turns rows into buckets. The JWT never leaves this
//! crate except as `account_hash` / `account_label`.
//!
//! Collection is opt-in: only accounts the user joined (the agent secrets
//! file, `cursor_extra_accounts`) are collected. The IDE login is detected
//! for display but never auto-enrolled and never auto-refreshes a stored
//! token — the user controls what gets collected.

mod csv;
mod extract;
mod jwt;
mod local;
mod paths;
mod remote;

use std::collections::HashSet;
use std::path::PathBuf;

use ai_usage_protocol::{account_hash_from_sub, SOURCE_CURSOR};

use crate::util::entries_to_buckets;
use crate::{ParseCtx, ParseResult, UsageAdapter};

pub struct CursorAdapter;

/// No joined accounts: source is idle, not failing. Agent treats this skip as
/// "not enrolled" instead of an error.
pub const CURSOR_NOT_ENROLLED: &str = "Cursor: 未加入采集账号；在面板加入后开始采集。";
const SKIP_HINT: &str = "Cursor: 拉取用量失败（网络或服务端），本轮跳过。";
const CSV_HINT: &str = "Cursor: 用量 CSV 格式无法解析，本轮跳过。";
const EXTRA_BAD_HINT: &str = "Cursor: 已加入的凭证无法解析，请重新导入。";
const EXTRA_EXPIRED: &str = "会话已过期，请重新导入。";
const CONFIGURED_ACCOUNTS: &str = "cursor-accounts";

pub use extract::{
    credit_overlay, extract_cursor_previews, sand_overlay, snapshot_from_usage_json,
    CursorAccountSnapshot,
};

#[derive(Debug, Clone)]
pub struct CursorTokenPreview {
    pub access_token: String,
    pub account_hash: String,
    pub account_label: String,
    pub exp: Option<i64>,
    /// JWT `type` 声明：`web`（网站登录，Bot 不可用）或原生 token（None/其它）。
    pub token_type: Option<String>,
    pub snapshot: CursorAccountSnapshot,
}

/// Accept a raw JWT or a `WorkosCursorSessionToken` value (`sub::jwt`).
pub fn extract_cursor_jwt(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let jwt = split_session_cookie(raw).unwrap_or(raw);
    let jwt = jwt.trim();
    if !is_jwt_shape(jwt) {
        return None;
    }
    Some(jwt.to_string())
}

fn is_jwt_shape(jwt: &str) -> bool {
    let mut parts = jwt.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(a), Some(b), Some(c), None) if !a.is_empty() && !b.is_empty() && !c.is_empty()
    )
}

fn split_session_cookie(raw: &str) -> Option<&str> {
    let lower = raw.to_ascii_lowercase();
    if let Some(i) = lower.find("%3a%3a") {
        return Some(&raw[i + 6..]);
    }
    raw.split_once("::").map(|(_, jwt)| jwt)
}

pub fn preview_cursor_token(raw: &str) -> Option<CursorTokenPreview> {
    let access_token = extract_cursor_jwt(raw)?;
    let claims = jwt::decode_claims(&access_token)?;
    let account_hash = account_hash_from_sub(&claims.sub);
    let account_label = account_label("", &claims.email, &account_hash);
    Some(CursorTokenPreview {
        access_token,
        account_hash,
        account_label,
        exp: claims.exp,
        token_type: claims.token_type,
        snapshot: CursorAccountSnapshot::default(),
    })
}

pub fn read_ide_cursor_auth(ctx: &ParseCtx) -> Option<CursorTokenPreview> {
    let db = paths::detect_state_db(ctx)?;
    let auth = local::read_auth(&db).ok()??;
    let mut preview = preview_cursor_token(&auth.access_token)?;
    if !auth.cached_email.trim().is_empty() {
        preview.account_label = auth.cached_email;
    }
    Some(preview)
}

/// Why `GET /api/usage-summary` failed. The panel maps these to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanFetchError {
    Token,
    Auth,
    Network,
    Status,
    Parse,
}

/// Live plan meter from `GET /api/usage-summary`. None = network/auth/parse failed.
pub fn fetch_plan_snapshot(access_token: &str) -> Option<CursorAccountSnapshot> {
    fetch_plan_with_raw(access_token).ok().map(|(snap, _)| snap)
}

/// Same as [`fetch_plan_snapshot`], plus the parsed `usage-summary` body.
///
/// 额外尝试 Bot/Sand 配额并叠加进快照：`type=web` 的凭证预知调不了原生
/// RPC，直接跳过；其余失败（401/网络）也只是 Bot 字段留空，不算错误。
/// Sand 原始 JSON 以 `botUsage` 键并入返回的 raw，供面板查看。
///
/// 信用余额不在这里拉（面板每分钟刷一次太频繁），见 [`fetch_credit_grants`]。
pub fn fetch_plan_with_raw(
    access_token: &str,
) -> Result<(CursorAccountSnapshot, serde_json::Value), PlanFetchError> {
    let jwt = extract_cursor_jwt(access_token).ok_or(PlanFetchError::Token)?;
    let claims = jwt::decode_claims(&jwt).ok_or(PlanFetchError::Token)?;
    let raw = remote::fetch_usage_summary(&claims.sub, &jwt).map_err(map_fetch_err)?;
    let mut v: serde_json::Value = serde_json::from_str(&raw).map_err(|_| PlanFetchError::Parse)?;
    let mut snap = snapshot_from_usage_json(&v);
    if claims.token_type.as_deref() != Some("web") {
        if let Ok(sand_raw) = remote::fetch_sand_usage(&jwt) {
            if let Ok(sand) = serde_json::from_str::<serde_json::Value>(&sand_raw) {
                sand_overlay(&mut snap, &sand);
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("botUsage".into(), sand);
                }
            }
        }
    }
    Ok((snap, v))
}

/// 信用余额（网页 Credits 卡）：`POST get-client-visible-credit-grants` 的原始
/// JSON。`usage-summary` 里没有这项。余额变化慢，调用方按 Cursor 同步周期
/// （与全量 CSV 同频）拉取即可；叠加进快照用 [`credit_overlay`]。
pub fn fetch_credit_grants(access_token: &str) -> Result<serde_json::Value, PlanFetchError> {
    let jwt = extract_cursor_jwt(access_token).ok_or(PlanFetchError::Token)?;
    let claims = jwt::decode_claims(&jwt).ok_or(PlanFetchError::Token)?;
    let raw = remote::fetch_credit_grants(&claims.sub, &jwt).map_err(map_fetch_err)?;
    serde_json::from_str(&raw).map_err(|_| PlanFetchError::Parse)
}

fn map_fetch_err(err: remote::FetchError) -> PlanFetchError {
    match err {
        remote::FetchError::Auth => PlanFetchError::Auth,
        remote::FetchError::Network => PlanFetchError::Network,
        remote::FetchError::Status => PlanFetchError::Status,
    }
}

/// 账号的一条登录会话（`GET /api/auth/sessions`）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CursorSession {
    pub session_id: String,
    /// 服务端枚举名：`SESSION_TYPE_WEB` / `SESSION_TYPE_CLIENT` / …
    pub session_type: String,
    /// 撤销接口要的数字枚举；未知类型为 None（不可撤销）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_code: Option<u32>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// 这条就是采集端正在用的凭证：JWT `time` 声明与 `createdAt` 同秒。
    pub current: bool,
}

/// 前端 `authSessionTypeRevokeValue` 的映射。
pub fn session_type_code(session_type: &str) -> Option<u32> {
    match session_type {
        "SESSION_TYPE_WEB" => Some(1),
        "SESSION_TYPE_CLIENT" => Some(2),
        "SESSION_TYPE_MOBILE" => Some(10),
        "SESSION_TYPE_CHROME_EXTENSION" => Some(11),
        _ => None,
    }
}

/// 拉取账号全部登录会话，并标出采集端自己那条。
pub fn fetch_sessions(access_token: &str) -> Result<Vec<CursorSession>, PlanFetchError> {
    let jwt = extract_cursor_jwt(access_token).ok_or(PlanFetchError::Token)?;
    let claims = jwt::decode_claims(&jwt).ok_or(PlanFetchError::Token)?;
    let raw = remote::fetch_sessions(&claims.sub, &jwt).map_err(map_fetch_err)?;
    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|_| PlanFetchError::Parse)?;
    Ok(sessions_from_json(&v, claims.issued_at))
}

fn sessions_from_json(v: &serde_json::Value, issued_at: Option<i64>) -> Vec<CursorSession> {
    let Some(items) = v.get("sessions").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|s| {
            let obj = s.as_object()?;
            let session_id = obj.get("sessionId")?.as_str()?.to_string();
            let session_type = obj
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let created_at = obj
                .get("createdAt")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let expires_at = obj
                .get("expiresAt")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let current = match (issued_at, chrono::DateTime::parse_from_rfc3339(&created_at)) {
                (Some(t), Ok(dt)) => dt.timestamp() == t,
                _ => false,
            };
            Some(CursorSession {
                type_code: session_type_code(&session_type),
                session_id,
                session_type,
                created_at,
                expires_at,
                current,
            })
        })
        .collect()
}

/// 撤销一条会话。`session_type` 传服务端枚举名。
pub fn revoke_session(
    access_token: &str,
    session_id: &str,
    session_type: &str,
) -> Result<(), PlanFetchError> {
    let jwt = extract_cursor_jwt(access_token).ok_or(PlanFetchError::Token)?;
    let claims = jwt::decode_claims(&jwt).ok_or(PlanFetchError::Token)?;
    let code = session_type_code(session_type).ok_or(PlanFetchError::Parse)?;
    remote::revoke_session(&claims.sub, &jwt, session_id, code)
        .map(|_| ())
        .map_err(map_fetch_err)
}

impl UsageAdapter for CursorAdapter {
    fn id(&self) -> &'static str {
        SOURCE_CURSOR
    }

    fn detect(&self, ctx: &ParseCtx) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = paths::detect_state_db(ctx).into_iter().collect();
        if out.is_empty() && !ctx.env.cursor_extra_accounts.is_empty() {
            out.push(PathBuf::from(CONFIGURED_ACCOUNTS));
        }
        out
    }

    fn parse(&self, ctx: &ParseCtx) -> ParseResult {
        parse_with_fetch(ctx, remote::fetch_usage_csv)
    }
}

struct Cred {
    token: String,
    cached_email: String,
    report_since: Option<chrono::DateTime<chrono::Utc>>,
}

/// 只采集已加入的账号（`cursor_extra_accounts`）。IDE 登录不自动计入——
/// 用户在面板「加入采集」后，其凭证才会出现在这里（严格用加入时那份 JWT）。
fn parse_with_fetch(
    ctx: &ParseCtx,
    fetch: impl Fn(&str, &str) -> Result<String, remote::FetchError>,
) -> ParseResult {
    let mut warnings = Vec::new();

    if ctx.env.cursor_extra_accounts.is_empty() {
        return skipped(CURSOR_NOT_ENROLLED);
    }

    let mut creds = Vec::new();
    for extra in &ctx.env.cursor_extra_accounts {
        creds.push(Cred {
            token: extra.access_token.clone(),
            cached_email: extra.account_label.clone(),
            report_since: extra.report_since,
        });
    }

    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for cred in creds {
        let Some(claims) = jwt::decode_claims(&cred.token) else {
            warnings.push(EXTRA_BAD_HINT.to_string());
            continue;
        };
        let hash = account_hash_from_sub(&claims.sub);
        if !seen.insert(hash) {
            continue;
        }
        unique.push((cred, claims));
    }

    if unique.is_empty() {
        return ParseResult {
            skipped: true,
            warnings,
            ..ParseResult::default()
        };
    }

    let mut buckets = Vec::new();
    let mut attempted = Vec::new();
    let mut succeeded = Vec::new();

    for (cred, claims) in unique {
        let account_hash = account_hash_from_sub(&claims.sub);
        let label = account_label(&cred.cached_email, &claims.email, &account_hash);
        attempted.push(account_hash.clone());
        let csv = match fetch(&claims.sub, &cred.token) {
            Ok(text) => text,
            Err(remote::FetchError::Auth) => {
                warnings.push(format!("Cursor: {label} {EXTRA_EXPIRED}"));
                continue;
            }
            Err(remote::FetchError::Network | remote::FetchError::Status) => {
                warnings.push(format!(
                    "Cursor: {label} {}",
                    SKIP_HINT.trim_start_matches("Cursor: ")
                ));
                continue;
            }
        };
        let mut entries = match csv::parse_export_csv(&csv) {
            Ok(rows) => rows,
            Err(_) => {
                warnings.push(format!(
                    "Cursor: {label} {}",
                    CSV_HINT.trim_start_matches("Cursor: ")
                ));
                continue;
            }
        };
        // 统计起始为固定 cutoff：cutoff 前的条目永不进 live 集，
        // state 修剪一次后稳定，不会像滚动窗那样反复重传。
        if let Some(since) = cred.report_since {
            entries.retain(|e| e.timestamp >= since);
        }
        let mut rows = entries_to_buckets(&entries);
        for b in &mut rows {
            b.account_hash = account_hash.clone();
            b.account_label = label.clone();
        }
        buckets.extend(rows);
        succeeded.push(account_hash);
    }

    if succeeded.is_empty() {
        return ParseResult {
            skipped: true,
            warnings,
            attempted_account_hashes: attempted,
            succeeded_account_hashes: succeeded,
            ..ParseResult::default()
        };
    }

    ParseResult {
        buckets,
        sessions: Vec::new(),
        skipped: false,
        warnings,
        attempted_account_hashes: attempted,
        succeeded_account_hashes: succeeded,
    }
}

fn account_label(cached_email: &str, jwt_email: &str, account_hash: &str) -> String {
    let cached = cached_email.trim();
    if !cached.is_empty() {
        return cached.to_string();
    }
    let jwt = jwt_email.trim();
    if !jwt.is_empty() {
        return jwt.to_string();
    }
    account_hash.chars().take(8).collect()
}

fn skipped(warning: &str) -> ParseResult {
    ParseResult {
        skipped: true,
        warnings: vec![warning.to_string()],
        ..ParseResult::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AdapterEnv, CursorExtraAccount, ParseCtx};
    use ai_usage_protocol::account_hash_from_sub;
    use std::fs;

    fn fixture_csv() -> String {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/cursor/export.csv");
        fs::read_to_string(path).expect("fixtures/cursor/export.csv")
    }

    #[test]
    fn plan_fetch_rejects_garbage_token() {
        assert_eq!(fetch_plan_with_raw("nope").unwrap_err(), PlanFetchError::Token);
        assert!(fetch_plan_snapshot("nope").is_none());
        assert_eq!(
            fetch_credit_grants("nope").unwrap_err(),
            PlanFetchError::Token
        );
    }

    #[test]
    fn sessions_json_marks_current_by_issued_at() {
        let v = serde_json::json!({"sessions": [
            {"sessionId":"aaa","type":"SESSION_TYPE_WEB","createdAt":"2026-08-21T12:22:57.000Z","expiresAt":"2026-10-20T12:22:57.000Z"},
            {"sessionId":"bbb","type":"SESSION_TYPE_CLIENT","createdAt":"2026-09-03T09:46:13.000Z","expiresAt":"2026-11-02T09:46:13.000Z"},
            {"sessionId":"ccc","type":"SESSION_TYPE_FUTURE","createdAt":"2026-09-03T10:00:00.000Z"}
        ]});
        let out = sessions_from_json(&v, Some(1788428773));
        assert_eq!(out.len(), 3);
        assert!(!out[0].current);
        assert_eq!(out[0].type_code, Some(1));
        assert!(out[1].current);
        assert_eq!(out[1].type_code, Some(2));
        assert_eq!(out[2].type_code, None);
        assert!(out[2].expires_at.is_none());
        assert!(sessions_from_json(&v, None).iter().all(|s| !s.current));
        assert!(sessions_from_json(&serde_json::json!({}), None).is_empty());
    }

    #[test]
    fn revoke_rejects_unknown_type_without_network() {
        let token = jwt::fake_jwt("user_r", "r@x.com");
        assert_eq!(
            revoke_session(&token, "abc", "SESSION_TYPE_FUTURE").unwrap_err(),
            PlanFetchError::Parse
        );
        assert_eq!(
            revoke_session("nope", "abc", "SESSION_TYPE_WEB").unwrap_err(),
            PlanFetchError::Token
        );
    }

    #[test]
    fn label_prefers_cached_email_then_jwt_then_short_hash() {
        let hash = account_hash_from_sub("user_1");
        let short: String = hash.chars().take(8).collect();
        assert_eq!(account_label("a@x.com", "b@y.com", &hash), "a@x.com");
        assert_eq!(account_label("", "b@y.com", &hash), "b@y.com");
        assert_eq!(account_label("  ", "", &hash), short);
    }

    #[test]
    fn extract_jwt_from_cookie_forms() {
        let jwt = jwt::fake_jwt("user_01", "a@x.com");
        assert_eq!(extract_cursor_jwt(&jwt).as_deref(), Some(jwt.as_str()));
        assert_eq!(
            extract_cursor_jwt(&format!("user_01::{jwt}")).as_deref(),
            Some(jwt.as_str())
        );
        assert_eq!(
            extract_cursor_jwt(&format!("user_01%3A%3A{jwt}")).as_deref(),
            Some(jwt.as_str())
        );
        assert!(extract_cursor_jwt("not-a-token").is_none());
    }

    #[test]
    fn no_enrolled_accounts_is_idle_skip_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.vscdb");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT);")
            .unwrap();
        drop(conn);
        let ctx = ParseCtx {
            home: dir.path().to_path_buf(),
            cache_dir: dir.path().join("cache"),
            env: AdapterEnv {
                cursor_state_db: Some(db),
                ..AdapterEnv::default()
            },
        };
        let r = CursorAdapter.parse(&ctx);
        assert!(r.skipped);
        assert!(r.warnings.iter().any(|w| w == CURSOR_NOT_ENROLLED));
        assert!(r.buckets.is_empty());
        assert!(r.sessions.is_empty());
    }

    #[test]
    fn ide_login_is_not_auto_collected() {
        // IDE 已登录但未「加入采集」：不发任何网络请求，标记为未启用。
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.vscdb");
        let token = jwt::fake_jwt("user_ide", "ide@x.com");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT);")
            .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES('cursorAuth/accessToken', ?1)",
            rusqlite::params![token],
        )
        .unwrap();
        drop(conn);
        let ctx = ParseCtx {
            home: dir.path().to_path_buf(),
            cache_dir: dir.path().join("cache"),
            env: AdapterEnv {
                cursor_state_db: Some(db),
                ..AdapterEnv::default()
            },
        };
        let r = parse_with_fetch(&ctx, |_, _| panic!("must not fetch"));
        assert!(r.skipped);
        assert!(r.warnings.iter().any(|w| w == CURSOR_NOT_ENROLLED));
        assert!(r.attempted_account_hashes.is_empty());
    }

    #[test]
    fn report_since_is_fixed_cutoff() {
        let dir = tempfile::tempdir().unwrap();
        let token = jwt::fake_jwt("user_since", "s@x.com");
        let csv = fixture_csv();
        let mk_ctx = |since: Option<chrono::DateTime<chrono::Utc>>| ParseCtx {
            home: dir.path().to_path_buf(),
            cache_dir: dir.path().join("cache"),
            env: AdapterEnv {
                cursor_extra_accounts: vec![CursorExtraAccount {
                    access_token: token.clone(),
                    account_label: "s@x.com".into(),
                    report_since: since,
                }],
                ..AdapterEnv::default()
            },
        };
        let all = parse_with_fetch(&mk_ctx(None), |_, _| Ok(csv.clone()));
        assert!(!all.skipped);
        let total: i64 = all.buckets.iter().map(|b| b.total_tokens).sum();
        assert!(total > 0);
        // 起始设在 10:30 之后：只剩 10:45 那行
        let since = chrono::DateTime::parse_from_rfc3339("2026-01-15T10:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let cut = parse_with_fetch(&mk_ctx(Some(since)), |_, _| Ok(csv.clone()));
        assert!(!cut.skipped);
        let cut_total: i64 = cut.buckets.iter().map(|b| b.total_tokens).sum();
        assert!(cut_total > 0 && cut_total < total, "{cut_total} vs {total}");
        assert!(cut
            .buckets
            .iter()
            .all(|b| b.bucket_start >= since - chrono::Duration::minutes(30)));
        // 起始在未来：无条目但账号 attempted/succeeded 照记，state 不误剪
        let future = chrono::DateTime::parse_from_rfc3339("2099-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let none = parse_with_fetch(&mk_ctx(Some(future)), |_, _| Ok(csv.clone()));
        assert!(!none.skipped);
        assert!(none.buckets.is_empty());
        assert_eq!(none.succeeded_account_hashes.len(), 1);
    }

    #[test]
    fn enrolled_account_uses_stored_token_not_ide() {
        // 严格用加入时那份 JWT：IDE 换发新 token 后仍用存量凭证请求。
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.vscdb");
        let ide_token = jwt::fake_jwt("user_same", "s@x.com");
        let stored_token = jwt::fake_jwt_exp("user_same", "s@x.com", Some(1_700_000_000));
        assert_ne!(ide_token, stored_token);
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT);")
            .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES('cursorAuth/accessToken', ?1)",
            rusqlite::params![ide_token],
        )
        .unwrap();
        drop(conn);
        let csv = fixture_csv();
        let seen_tokens = std::sync::Mutex::new(Vec::new());
        let ctx = ParseCtx {
            home: dir.path().to_path_buf(),
            cache_dir: dir.path().join("cache"),
            env: AdapterEnv {
                cursor_state_db: Some(db),
                cursor_extra_accounts: vec![CursorExtraAccount {
                    access_token: stored_token.clone(),
                    account_label: "s@x.com".into(),
                    report_since: None,
                }],
                ..AdapterEnv::default()
            },
        };
        let r = parse_with_fetch(&ctx, |_, token| {
            seen_tokens.lock().unwrap().push(token.to_string());
            Ok(csv.clone())
        });
        assert!(!r.skipped);
        let tokens = seen_tokens.into_inner().unwrap();
        assert_eq!(tokens, vec![stored_token]);
    }

    #[test]
    fn detect_without_vscdb_when_extras_configured() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ParseCtx {
            home: dir.path().to_path_buf(),
            cache_dir: dir.path().join("cache"),
            env: AdapterEnv {
                cursor_extra_accounts: vec![CursorExtraAccount {
                    access_token: jwt::fake_jwt("user_x", "a@x.com"),
                    account_label: "a@x.com".into(),
                    report_since: None,
                }],
                ..AdapterEnv::default()
            },
        };
        assert_eq!(
            CursorAdapter.detect(&ctx),
            vec![PathBuf::from(CONFIGURED_ACCOUNTS)]
        );
    }

    #[test]
    fn extras_only_logged_out_ide_does_not_skip_source() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.vscdb");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT);")
            .unwrap();
        drop(conn);
        let csv = fixture_csv();
        let token = jwt::fake_jwt("user_extra", "e@x.com");
        let ctx = ParseCtx {
            home: dir.path().to_path_buf(),
            cache_dir: dir.path().join("cache"),
            env: AdapterEnv {
                cursor_state_db: Some(db),
                cursor_extra_accounts: vec![CursorExtraAccount {
                    access_token: token,
                    account_label: "e@x.com".into(),
                    report_since: None,
                }],
                ..AdapterEnv::default()
            },
        };
        let r = parse_with_fetch(&ctx, |_, _| Ok(csv.clone()));
        assert!(!r.skipped);
        assert!(!r.buckets.is_empty());
        assert_eq!(r.succeeded_account_hashes.len(), 1);
    }

    #[test]
    fn one_account_auth_failure_keeps_the_other() {
        let csv = fixture_csv();
        let good = jwt::fake_jwt("user_good", "a@x.com");
        let bad = jwt::fake_jwt("user_bad", "b@x.com");
        let dir = tempfile::tempdir().unwrap();
        let ctx = ParseCtx {
            home: dir.path().to_path_buf(),
            cache_dir: dir.path().join("cache"),
            env: AdapterEnv {
                cursor_extra_accounts: vec![
                    CursorExtraAccount {
                        access_token: good,
                        account_label: "a@x.com".into(),
                        report_since: None,
                    },
                    CursorExtraAccount {
                        access_token: bad,
                        account_label: "b@x.com".into(),
                        report_since: None,
                    },
                ],
                ..AdapterEnv::default()
            },
        };
        let r = parse_with_fetch(&ctx, |sub, _| {
            if sub == "user_bad" {
                Err(remote::FetchError::Auth)
            } else {
                Ok(csv.clone())
            }
        });
        assert!(!r.skipped);
        assert!(!r.buckets.is_empty());
        assert!(r.warnings.iter().any(|w| w.contains("重新导入")));
        assert_eq!(r.attempted_account_hashes.len(), 2);
        assert_eq!(r.succeeded_account_hashes.len(), 1);
        let good_hash = account_hash_from_sub("user_good");
        assert_eq!(r.succeeded_account_hashes, vec![good_hash.clone()]);
        assert!(r.buckets.iter().all(|b| b.account_hash == good_hash));
    }

    #[test]
    fn ide_and_extra_same_account_are_deduped() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.vscdb");
        let token = jwt::fake_jwt("user_same", "s@x.com");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT);")
            .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES('cursorAuth/accessToken', ?1)",
            rusqlite::params![token],
        )
        .unwrap();
        drop(conn);
        let csv = fixture_csv();
        let fetches = std::sync::atomic::AtomicUsize::new(0);
        let ctx = ParseCtx {
            home: dir.path().to_path_buf(),
            cache_dir: dir.path().join("cache"),
            env: AdapterEnv {
                cursor_state_db: Some(db),
                cursor_extra_accounts: vec![CursorExtraAccount {
                    access_token: token,
                    account_label: "s@x.com".into(),
                    report_since: None,
                }],
                ..AdapterEnv::default()
            },
        };
        let r = parse_with_fetch(&ctx, |_, _| {
            fetches.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(csv.clone())
        });
        assert!(!r.skipped);
        assert_eq!(fetches.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(r.attempted_account_hashes.len(), 1);
    }

    #[test]
    fn preview_reads_exp() {
        let token = jwt::fake_jwt_exp("user_01", "a@x.com", Some(1_800_000_000));
        let p = preview_cursor_token(&token).unwrap();
        assert_eq!(p.exp, Some(1_800_000_000));
        assert_eq!(p.account_label, "a@x.com");
    }
}
