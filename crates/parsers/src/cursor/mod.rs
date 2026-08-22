//! Cursor usage is the account CSV on cursor.com, not local bubbles.
//!
//! Layers: `paths` finds `state.vscdb` → `local` reads the JWT row → `remote`
//! fetches the CSV → `csv` turns rows into buckets. The JWT never leaves this
//! crate except as `account_hash` / `account_label`. Extra accounts come from
//! the agent secrets file, not from a second IDE login.

mod csv;
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

const LOGIN_HINT: &str = "Cursor: 未登录。请在 Cursor 中登录后再同步，或在采集端面板导入凭证。";
const RELLOGIN_HINT: &str = "Cursor: 会话已失效，请在 Cursor 中重新登录。";
const SKIP_HINT: &str = "Cursor: 拉取用量失败（网络或服务端），本轮跳过。";
const DB_HINT: &str = "Cursor: 无法读取本地登录状态，本轮跳过。";
const CSV_HINT: &str = "Cursor: 用量 CSV 格式无法解析，本轮跳过。";
const EXTRA_BAD_HINT: &str = "Cursor: 额外凭证无法解析，请重新导入。";
const EXTRA_EXPIRED: &str = "会话已过期，请重新导入。";
const CONFIGURED_ACCOUNTS: &str = "cursor-accounts";

#[derive(Debug, Clone)]
pub struct CursorTokenPreview {
    pub access_token: String,
    pub account_hash: String,
    pub account_label: String,
    pub exp: Option<i64>,
}

/// Accept a raw JWT or a `WorkosCursorSessionToken` value (`sub::jwt`).
pub fn extract_cursor_jwt(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let jwt = split_session_cookie(raw).unwrap_or(raw);
    let jwt = jwt.trim();
    if jwt.split('.').count() < 3 {
        return None;
    }
    Some(jwt.to_string())
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
    from_ide: bool,
}

fn parse_with_fetch(
    ctx: &ParseCtx,
    fetch: impl Fn(&str, &str) -> Result<String, remote::FetchError>,
) -> ParseResult {
    let mut warnings = Vec::new();
    let mut creds = Vec::new();

    if let Some(db) = paths::detect_state_db(ctx) {
        match local::read_auth(&db) {
            Ok(Some(auth)) => creds.push(Cred {
                token: auth.access_token,
                cached_email: auth.cached_email,
                from_ide: true,
            }),
            Ok(None) => {
                if ctx.env.cursor_extra_accounts.is_empty() {
                    return skipped(LOGIN_HINT);
                }
            }
            Err(_) => {
                if ctx.env.cursor_extra_accounts.is_empty() {
                    return skipped(DB_HINT);
                }
                warnings.push(DB_HINT.to_string());
            }
        }
    }

    for extra in &ctx.env.cursor_extra_accounts {
        creds.push(Cred {
            token: extra.access_token.clone(),
            cached_email: extra.account_label.clone(),
            from_ide: false,
        });
    }

    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for cred in creds {
        let Some(claims) = jwt::decode_claims(&cred.token) else {
            warnings.push(if cred.from_ide {
                RELLOGIN_HINT.to_string()
            } else {
                EXTRA_BAD_HINT.to_string()
            });
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
                warnings.push(if cred.from_ide {
                    RELLOGIN_HINT.to_string()
                } else {
                    format!("Cursor: {label} {EXTRA_EXPIRED}")
                });
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
        let entries = match csv::parse_export_csv(&csv) {
            Ok(rows) => rows,
            Err(_) => {
                warnings.push(format!(
                    "Cursor: {label} {}",
                    CSV_HINT.trim_start_matches("Cursor: ")
                ));
                continue;
            }
        };
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
    fn parse_logged_out_skips_without_network() {
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
        assert!(r.warnings.iter().any(|w| w.contains("未登录")));
        assert!(r.buckets.is_empty());
        assert!(r.sessions.is_empty());
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
                    },
                    CursorExtraAccount {
                        access_token: bad,
                        account_label: "b@x.com".into(),
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
