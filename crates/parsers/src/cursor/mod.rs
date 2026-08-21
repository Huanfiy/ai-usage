//! Cursor usage is the account CSV on cursor.com, not local bubbles.
//!
//! Layers: `paths` finds `state.vscdb` → `local` reads the JWT row → `remote`
//! fetches the CSV → `csv` turns rows into buckets. The JWT never leaves this
//! crate except as `account_hash` / `account_label`.

mod csv;
mod jwt;
mod local;
mod paths;
mod remote;

use ai_usage_protocol::{account_hash_from_sub, SOURCE_CURSOR};

use crate::util::entries_to_buckets;
use crate::{ParseCtx, ParseResult, UsageAdapter};

pub struct CursorAdapter;

const LOGIN_HINT: &str = "Cursor: 未登录。请在 Cursor 中登录后再同步。";
const RELLOGIN_HINT: &str = "Cursor: 会话已失效，请在 Cursor 中重新登录。";
const SKIP_HINT: &str = "Cursor: 拉取用量失败（网络或服务端），本轮跳过。";
const DB_HINT: &str = "Cursor: 无法读取本地登录状态，本轮跳过。";
const CSV_HINT: &str = "Cursor: 用量 CSV 格式无法解析，本轮跳过。";

impl UsageAdapter for CursorAdapter {
    fn id(&self) -> &'static str {
        SOURCE_CURSOR
    }

    fn detect(&self, ctx: &ParseCtx) -> Vec<std::path::PathBuf> {
        paths::detect_state_db(ctx).into_iter().collect()
    }

    fn parse(&self, ctx: &ParseCtx) -> ParseResult {
        let Some(db_path) = paths::detect_state_db(ctx) else {
            return ParseResult::default();
        };
        let auth = match local::read_auth(&db_path) {
            Ok(Some(auth)) => auth,
            Ok(None) => {
                return skipped(LOGIN_HINT);
            }
            Err(_) => return skipped(DB_HINT),
        };
        let Some(claims) = jwt::decode_claims(&auth.access_token) else {
            return skipped(RELLOGIN_HINT);
        };
        let account_hash = account_hash_from_sub(&claims.sub);
        let account_label = account_label(&auth.cached_email, &claims.email, &account_hash);

        let csv = match remote::fetch_usage_csv(&claims.sub, &auth.access_token) {
            Ok(text) => text,
            Err(remote::FetchError::Auth) => return skipped(RELLOGIN_HINT),
            Err(remote::FetchError::Network | remote::FetchError::Status) => {
                return skipped(SKIP_HINT);
            }
        };

        let entries = match csv::parse_export_csv(&csv) {
            Ok(rows) => rows,
            Err(_) => return skipped(CSV_HINT),
        };
        let mut buckets = entries_to_buckets(&entries);
        for b in &mut buckets {
            b.account_hash = account_hash.clone();
            b.account_label = account_label.clone();
        }
        ParseResult {
            buckets,
            sessions: Vec::new(),
            skipped: false,
            warnings: Vec::new(),
        }
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
    use crate::{AdapterEnv, ParseCtx};
    use ai_usage_protocol::account_hash_from_sub;

    #[test]
    fn label_prefers_cached_email_then_jwt_then_short_hash() {
        let hash = account_hash_from_sub("user_1");
        let short: String = hash.chars().take(8).collect();
        assert_eq!(account_label("a@x.com", "b@y.com", &hash), "a@x.com");
        assert_eq!(account_label("", "b@y.com", &hash), "b@y.com");
        assert_eq!(account_label("  ", "", &hash), short);
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
}
