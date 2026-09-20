use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).context("打开 SQLite 失败")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let g = self.conn.lock().expect("db poisoned");
        f(&g)
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS hosts (
  host_id TEXT PRIMARY KEY,
  hostname TEXT NOT NULL,
  last_seen TEXT NOT NULL,
  agent_version TEXT,
  timezone TEXT
);
CREATE TABLE IF NOT EXISTS ingest_tokens (
  token_hash TEXT PRIMARY KEY,
  host_id TEXT NOT NULL UNIQUE,
  token_prefix TEXT NOT NULL,
  label TEXT,
  created_at TEXT NOT NULL,
  revoked_at TEXT,
  FOREIGN KEY (host_id) REFERENCES hosts(host_id)
);
CREATE TABLE IF NOT EXISTS usage_buckets (
  host_id TEXT NOT NULL,
  source TEXT NOT NULL,
  model TEXT NOT NULL,
  project TEXT NOT NULL,
  bucket_start TEXT NOT NULL,
  input_tokens INTEGER NOT NULL,
  output_tokens INTEGER NOT NULL,
  cache_read_input_tokens INTEGER NOT NULL,
  cache_creation_input_tokens INTEGER NOT NULL,
  reasoning_output_tokens INTEGER NOT NULL,
  total_tokens INTEGER NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (host_id, source, model, project, bucket_start)
);
CREATE TABLE IF NOT EXISTS usage_sessions (
  host_id TEXT NOT NULL,
  source TEXT NOT NULL,
  session_hash TEXT NOT NULL,
  project TEXT NOT NULL,
  first_message_at TEXT NOT NULL,
  last_message_at TEXT NOT NULL,
  duration_seconds INTEGER NOT NULL,
  active_seconds INTEGER NOT NULL,
  message_count INTEGER NOT NULL,
  user_message_count INTEGER NOT NULL,
  input_tokens INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  cache_read_input_tokens INTEGER NOT NULL DEFAULT 0,
  cache_creation_input_tokens INTEGER NOT NULL DEFAULT 0,
  reasoning_output_tokens INTEGER NOT NULL DEFAULT 0,
  total_tokens INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (host_id, source, session_hash)
);
CREATE TABLE IF NOT EXISTS daily_rollups (
  day TEXT NOT NULL,
  host_id TEXT NOT NULL,
  source TEXT NOT NULL,
  model TEXT NOT NULL,
  project TEXT NOT NULL,
  input_tokens INTEGER NOT NULL,
  output_tokens INTEGER NOT NULL,
  cache_read_input_tokens INTEGER NOT NULL,
  cache_creation_input_tokens INTEGER NOT NULL,
  reasoning_output_tokens INTEGER NOT NULL,
  total_tokens INTEGER NOT NULL,
  PRIMARY KEY (day, host_id, source, model, project)
);
CREATE TABLE IF NOT EXISTS cursor_account_usage (
  account_hash TEXT PRIMARY KEY,
  account_label TEXT NOT NULL,
  membership TEXT,
  billing_cycle_start TEXT,
  billing_cycle_end TEXT,
  api_percent REAL,
  auto_percent REAL,
  bot_percent REAL,
  bot_period_start TEXT,
  bot_next_reset TEXT,
  bot_available INTEGER,
  total_used_cents INTEGER,
  fetched_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  archived_at TEXT
);
CREATE TABLE IF NOT EXISTS join_requests (
  join_id TEXT PRIMARY KEY,
  claim_hash TEXT NOT NULL,
  confirm_pin TEXT NOT NULL,
  hostname TEXT NOT NULL,
  agent_version TEXT,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  token TEXT,
  host_id TEXT
);
CREATE INDEX IF NOT EXISTS idx_join_status ON join_requests(status, expires_at);
CREATE INDEX IF NOT EXISTS idx_buckets_start ON usage_buckets(bucket_start);
CREATE INDEX IF NOT EXISTS idx_buckets_host ON usage_buckets(host_id, bucket_start);
CREATE INDEX IF NOT EXISTS idx_rollups_day ON daily_rollups(day);
CREATE INDEX IF NOT EXISTS idx_sessions_last ON usage_sessions(last_message_at);
"#;

/// 老库补列：`CREATE TABLE IF NOT EXISTS` 不会改已有表，这里逐列 `ADD COLUMN`。
fn migrate(conn: &Connection) -> Result<()> {
    add_missing_columns(
        conn,
        "usage_sessions",
        &[
            ("input_tokens", "INTEGER NOT NULL DEFAULT 0"),
            ("output_tokens", "INTEGER NOT NULL DEFAULT 0"),
            ("cache_read_input_tokens", "INTEGER NOT NULL DEFAULT 0"),
            ("cache_creation_input_tokens", "INTEGER NOT NULL DEFAULT 0"),
            ("reasoning_output_tokens", "INTEGER NOT NULL DEFAULT 0"),
            ("total_tokens", "INTEGER NOT NULL DEFAULT 0"),
        ],
    )?;
    add_missing_columns(conn, "hosts", &[("timezone", "TEXT")])?;
    add_missing_columns(
        conn,
        "cursor_account_usage",
        &[
            ("total_used_cents", "INTEGER"),
            ("archived_at", "TEXT"),
            ("billing_cycle_start", "TEXT"),
        ],
    )
}

fn add_missing_columns(conn: &Connection, table: &str, cols: &[(&str, &str)]) -> Result<()> {
    let existing: std::collections::HashSet<String> = conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<_>>()?;
    for (name, ty) in cols {
        if !existing.contains(*name) {
            conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {name} {ty}"), [])?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TokenRow {
    pub token_hash: String,
    pub host_id: String,
    pub token_prefix: String,
    pub label: Option<String>,
    pub revoked: bool,
}

pub fn lookup_token(conn: &Connection, token_hash: &str) -> Result<Option<TokenRow>> {
    let row = conn
        .query_row(
            "SELECT token_hash, host_id, token_prefix, label, revoked_at FROM ingest_tokens WHERE token_hash = ?1",
            params![token_hash],
            |r| {
                Ok(TokenRow {
                    token_hash: r.get(0)?,
                    host_id: r.get(1)?,
                    token_prefix: r.get(2)?,
                    label: r.get(3)?,
                    revoked: r.get::<_, Option<String>>(4)?.is_some(),
                })
            },
        )
        .optional()?;
    Ok(row)
}

pub fn insert_token(
    conn: &Connection,
    token_hash: &str,
    host_id: &str,
    token_prefix: &str,
    label: Option<&str>,
    hostname: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR IGNORE INTO hosts(host_id, hostname, last_seen, agent_version) VALUES(?1,?2,?3,NULL)",
        params![host_id, hostname, now],
    )?;
    conn.execute(
        "INSERT INTO ingest_tokens(token_hash, host_id, token_prefix, label, created_at, revoked_at)
         VALUES(?1,?2,?3,?4,?5,NULL)",
        params![token_hash, host_id, token_prefix, label, now],
    )?;
    Ok(())
}

pub fn list_tokens(conn: &Connection) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT t.token_prefix, t.host_id, t.label, t.created_at, t.revoked_at, h.hostname
         FROM ingest_tokens t JOIN hosts h ON h.host_id = t.host_id
         ORDER BY t.created_at DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(serde_json::json!({
                "token_prefix": r.get::<_, String>(0)?,
                "host_id": r.get::<_, String>(1)?,
                "label": r.get::<_, Option<String>>(2)?,
                "created_at": r.get::<_, String>(3)?,
                "revoked_at": r.get::<_, Option<String>>(4)?,
                "hostname": r.get::<_, String>(5)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn revoke_token(conn: &Connection, host_id: &str) -> Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let n = conn.execute(
        "UPDATE ingest_tokens SET revoked_at = ?1 WHERE host_id = ?2 AND revoked_at IS NULL",
        params![now, host_id],
    )?;
    Ok(n > 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteHostOutcome {
    Deleted,
    NotFound,
    NotRevoked,
    AccountScoped,
}

/// Remove a revoked machine host and its usage. Leaves other hosts and `acct:` rows untouched.
pub fn delete_host(conn: &Connection, host_id: &str) -> Result<DeleteHostOutcome> {
    if host_id.starts_with("acct:") {
        return Ok(DeleteHostOutcome::AccountScoped);
    }
    let revoked_at: Option<Option<String>> = conn
        .query_row(
            "SELECT revoked_at FROM ingest_tokens WHERE host_id = ?1",
            params![host_id],
            |r| r.get(0),
        )
        .optional()?;
    match revoked_at {
        None => return Ok(DeleteHostOutcome::NotFound),
        Some(None) => return Ok(DeleteHostOutcome::NotRevoked),
        Some(Some(_)) => {}
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM usage_buckets WHERE host_id = ?1",
        params![host_id],
    )?;
    tx.execute(
        "DELETE FROM usage_sessions WHERE host_id = ?1",
        params![host_id],
    )?;
    tx.execute(
        "DELETE FROM daily_rollups WHERE host_id = ?1",
        params![host_id],
    )?;
    tx.execute(
        "DELETE FROM ingest_tokens WHERE host_id = ?1",
        params![host_id],
    )?;
    tx.execute("DELETE FROM hosts WHERE host_id = ?1", params![host_id])?;
    tx.commit()?;
    Ok(DeleteHostOutcome::Deleted)
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct JoinRow {
    pub join_id: String,
    pub claim_hash: String,
    pub confirm_pin: String,
    pub hostname: String,
    pub agent_version: Option<String>,
    pub status: String,
    pub created_at: String,
    pub expires_at: String,
    pub token: Option<String>,
    pub host_id: Option<String>,
}

fn map_join_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<JoinRow> {
    Ok(JoinRow {
        join_id: r.get(0)?,
        claim_hash: r.get(1)?,
        confirm_pin: r.get(2)?,
        hostname: r.get(3)?,
        agent_version: r.get(4)?,
        status: r.get(5)?,
        created_at: r.get(6)?,
        expires_at: r.get(7)?,
        token: r.get(8)?,
        host_id: r.get(9)?,
    })
}

pub fn insert_join(
    conn: &Connection,
    join_id: &str,
    claim_hash: &str,
    confirm_pin: &str,
    hostname: &str,
    agent_version: Option<&str>,
    created_at: &str,
    expires_at: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO join_requests(
            join_id, claim_hash, confirm_pin, hostname, agent_version,
            status, created_at, expires_at, token, host_id
         ) VALUES(?1,?2,?3,?4,?5,'pending',?6,?7,NULL,NULL)",
        params![
            join_id,
            claim_hash,
            confirm_pin,
            hostname,
            agent_version,
            created_at,
            expires_at
        ],
    )?;
    Ok(())
}

pub fn get_join(conn: &Connection, join_id: &str) -> Result<Option<JoinRow>> {
    let row = conn
        .query_row(
            "SELECT join_id, claim_hash, confirm_pin, hostname, agent_version,
                    status, created_at, expires_at, token, host_id
             FROM join_requests WHERE join_id = ?1",
            params![join_id],
            map_join_row,
        )
        .optional()?;
    Ok(row)
}

pub fn pending_join_count(conn: &Connection, now: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM join_requests WHERE status = 'pending' AND expires_at > ?1",
        params![now],
        |r| r.get(0),
    )?)
}

pub fn list_pending_joins(conn: &Connection, now: &str) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT join_id, confirm_pin, hostname, agent_version, created_at, expires_at
         FROM join_requests
         WHERE status = 'pending' AND expires_at > ?1
         ORDER BY created_at ASC",
    )?;
    let rows = stmt
        .query_map(params![now], |r| {
            Ok(serde_json::json!({
                "join_id": r.get::<_, String>(0)?,
                "confirm_pin": r.get::<_, String>(1)?,
                "hostname": r.get::<_, String>(2)?,
                "agent_version": r.get::<_, Option<String>>(3)?,
                "created_at": r.get::<_, String>(4)?,
                "expires_at": r.get::<_, String>(5)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Expire stale pending/approved-unclaimed rows. Approved leftovers revoke the issued token.
pub fn expire_stale_joins(conn: &Connection, now: &str) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT join_id, status, host_id FROM join_requests
         WHERE expires_at <= ?1 AND status IN ('pending', 'approved')",
    )?;
    let stale: Vec<(String, String, Option<String>)> = stmt
        .query_map(params![now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for (join_id, status, host_id) in stale {
        if status == "approved" {
            if let Some(hid) = host_id {
                let _ = revoke_token(conn, &hid);
            }
        }
        conn.execute(
            "UPDATE join_requests SET status = 'expired', token = NULL WHERE join_id = ?1",
            params![join_id],
        )?;
    }
    Ok(())
}

pub fn approve_join(conn: &Connection, join_id: &str, token: &str, host_id: &str) -> Result<bool> {
    let n = conn.execute(
        "UPDATE join_requests SET status = 'approved', token = ?2, host_id = ?3
         WHERE join_id = ?1 AND status = 'pending'",
        params![join_id, token, host_id],
    )?;
    Ok(n > 0)
}

pub fn deny_join(conn: &Connection, join_id: &str) -> Result<bool> {
    let n = conn.execute(
        "UPDATE join_requests SET status = 'denied', token = NULL
         WHERE join_id = ?1 AND status = 'pending'",
        params![join_id],
    )?;
    Ok(n > 0)
}

/// Hand the plaintext token to the claimer once, then wipe it.
pub fn claim_join(conn: &Connection, join_id: &str) -> Result<Option<(String, String)>> {
    let row = match get_join(conn, join_id)? {
        Some(r) if r.status == "approved" => r,
        _ => return Ok(None),
    };
    let token = row.token.filter(|t| !t.is_empty());
    let host_id = row.host_id.filter(|h| !h.is_empty());
    let (token, host_id) = match (token, host_id) {
        (Some(t), Some(h)) => (t, h),
        _ => return Ok(None),
    };
    conn.execute(
        "UPDATE join_requests SET status = 'claimed', token = NULL WHERE join_id = ?1",
        params![join_id],
    )?;
    Ok(Some((token, host_id)))
}

/// 手动归档一个 Cursor 账号卡片；返回是否命中该账号。
pub fn archive_cursor_account(conn: &Connection, account_hash: &str) -> Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let n = conn.execute(
        "UPDATE cursor_account_usage SET archived_at = ?1
         WHERE account_hash = ?2 AND archived_at IS NULL",
        params![now, account_hash],
    )?;
    Ok(n > 0)
}

/// 手动恢复一个已归档的 Cursor 账号卡片；返回是否命中该账号。
pub fn restore_cursor_account(conn: &Connection, account_hash: &str) -> Result<bool> {
    let n = conn.execute(
        "UPDATE cursor_account_usage SET archived_at = NULL
         WHERE account_hash = ?1 AND archived_at IS NOT NULL",
        params![account_hash],
    )?;
    Ok(n > 0)
}

/// 全部 Cursor 账号的最新套餐快照，按显示名排序。
/// `stale_days`：快照超过该天数未更新则标记 `stale`（所有采集端都不再上报它）。
pub fn list_cursor_accounts(conn: &Connection, stale_days: u32) -> Result<Vec<serde_json::Value>> {
    let now = chrono::Utc::now();
    let stale_after = chrono::Duration::days(i64::from(stale_days));
    let mut stmt = conn.prepare(
        "SELECT account_hash, account_label, membership, billing_cycle_end,
                api_percent, auto_percent, bot_percent, bot_period_start, bot_next_reset,
                bot_available, total_used_cents, fetched_at, updated_at, archived_at,
                billing_cycle_start
         FROM cursor_account_usage
         ORDER BY account_label COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let fetched_at = r.get::<_, String>(11)?;
            let stale = chrono::DateTime::parse_from_rfc3339(&fetched_at)
                .map(|t| now - t.with_timezone(&chrono::Utc) > stale_after)
                .unwrap_or(false);
            Ok(serde_json::json!({
                "account_hash": r.get::<_, String>(0)?,
                "account_label": r.get::<_, String>(1)?,
                "membership": r.get::<_, Option<String>>(2)?,
                "billing_cycle_start": r.get::<_, Option<String>>(14)?,
                "billing_cycle_end": r.get::<_, Option<String>>(3)?,
                "api_percent": r.get::<_, Option<f64>>(4)?,
                "auto_percent": r.get::<_, Option<f64>>(5)?,
                "bot_percent": r.get::<_, Option<f64>>(6)?,
                "bot_period_start": r.get::<_, Option<String>>(7)?,
                "bot_next_reset": r.get::<_, Option<String>>(8)?,
                "bot_available": r.get::<_, Option<i64>>(9)?.map(|v| v != 0),
                "total_used_cents": r.get::<_, Option<i64>>(10)?,
                "fetched_at": fetched_at,
                "updated_at": r.get::<_, String>(12)?,
                "archived_at": r.get::<_, Option<String>>(13)?,
                "stale": stale,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::ingest;
    use ai_usage_protocol::{
        account_hash_from_sub, account_host_id, round_to_half_hour, IngestRequest, UsageBucket,
        UsageSession, SOURCE_CURSOR,
    };
    use chrono::{TimeZone, Utc};

    fn machine_bucket(tokens: i64) -> UsageBucket {
        let ts = Utc.with_ymd_and_hms(2026, 1, 15, 10, 17, 0).unwrap();
        UsageBucket {
            source: "codex".into(),
            model: "gpt-5.4".into(),
            project: "demo".into(),
            bucket_start: round_to_half_hour(ts),
            input_tokens: tokens,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: tokens,
            account_hash: String::new(),
            account_label: String::new(),
        }
    }

    fn cursor_bucket(hash: &str, tokens: i64) -> UsageBucket {
        let ts = Utc.with_ymd_and_hms(2026, 1, 15, 10, 17, 0).unwrap();
        UsageBucket {
            source: SOURCE_CURSOR.into(),
            model: "composer-1".into(),
            project: "unknown".into(),
            bucket_start: round_to_half_hour(ts),
            input_tokens: tokens,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: tokens,
            account_hash: hash.into(),
            account_label: "a@x.com".into(),
        }
    }

    #[test]
    fn legacy_cursor_account_usage_gains_new_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sqlite");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE cursor_account_usage (
                   account_hash TEXT PRIMARY KEY, account_label TEXT NOT NULL,
                   membership TEXT, billing_cycle_end TEXT,
                   api_percent REAL, auto_percent REAL, bot_percent REAL,
                   bot_period_start TEXT, bot_next_reset TEXT, bot_available INTEGER,
                   fetched_at TEXT NOT NULL, updated_at TEXT NOT NULL);
                 INSERT INTO cursor_account_usage(account_hash, account_label, fetched_at, updated_at)
                 VALUES('abc', 'a@x.com', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        db.with(|c| {
            let rows = list_cursor_accounts(c, 7)?;
            assert_eq!(rows.len(), 1);
            // 迁移补出 total_used_cents / archived_at / billing_cycle_start 列；
            // 旧快照（2026-01-01）超过 7 天未更新即 stale
            assert!(rows[0]["total_used_cents"].is_null());
            assert!(rows[0]["archived_at"].is_null());
            assert!(rows[0]["billing_cycle_start"].is_null());
            assert_eq!(rows[0]["stale"], true);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn archive_and_restore_cursor_account() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            let h = "a".repeat(32);
            let now = chrono::Utc::now().to_rfc3339();
            c.execute(
                "INSERT INTO cursor_account_usage(account_hash, account_label, fetched_at, updated_at)
                 VALUES(?1, 'a@x.com', ?2, ?2)",
                params![h, now],
            )?;
            // 未知账号不命中
            assert!(!archive_cursor_account(c, "unknown")?);
            // 归档：首次命中，重复归档不命中
            assert!(archive_cursor_account(c, &h)?);
            assert!(!archive_cursor_account(c, &h)?);
            let rows = list_cursor_accounts(c, 7)?;
            assert!(rows[0]["archived_at"].is_string());
            assert_eq!(rows[0]["stale"], false, "刚上报的快照不算 stale");
            // 恢复：首次命中，重复恢复不命中
            assert!(restore_cursor_account(c, &h)?);
            assert!(!restore_cursor_account(c, &h)?);
            let rows = list_cursor_accounts(c, 7)?;
            assert!(rows[0]["archived_at"].is_null());
            Ok(())
        })
        .unwrap();
    }

    fn session() -> UsageSession {
        let at = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        UsageSession {
            source: "codex".into(),
            project: "demo".into(),
            session_hash: "sess-a".into(),
            first_message_at: at,
            last_message_at: at,
            duration_seconds: 60,
            active_seconds: 40,
            message_count: 4,
            user_message_count: 2,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 0,
        }
    }

    fn seed_token(c: &Connection, host_id: &str, hostname: &str) {
        insert_token(
            c,
            &format!("hash-{host_id}"),
            host_id,
            "aiu_test",
            None,
            hostname,
        )
        .unwrap();
    }

    fn ingest_one(c: &Connection, host: &str, hostname: &str, buckets: Vec<UsageBucket>) {
        ingest(
            c,
            host,
            hostname,
            Some("0.1.0"),
            None,
            IngestRequest {
                schema_version: 1,
                hostname: Some(hostname.into()),
                agent_version: Some("0.1.0".into()),
                timezone: None,
                buckets,
                sessions: vec![session()],
                cursor_accounts: vec![],
            },
        )
        .unwrap();
    }

    fn count(c: &Connection, sql: &str, host_id: &str) -> i64 {
        c.query_row(sql, params![host_id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn delete_revoked_host_keeps_other_hosts_and_account_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            seed_token(c, "hostA", "pc1");
            seed_token(c, "hostB", "pc2");
            let hash = account_hash_from_sub("acct-alpha");
            let acct = account_host_id(&hash);
            ingest_one(
                c,
                "hostA",
                "pc1",
                vec![machine_bucket(40), cursor_bucket(&hash, 100)],
            );
            ingest_one(c, "hostB", "pc2", vec![machine_bucket(25)]);

            assert_eq!(delete_host(c, "hostA")?, DeleteHostOutcome::NotRevoked);
            assert!(revoke_token(c, "hostA")?);
            assert_eq!(delete_host(c, "hostA")?, DeleteHostOutcome::Deleted);
            assert_eq!(delete_host(c, "hostA")?, DeleteHostOutcome::NotFound);
            assert_eq!(delete_host(c, &acct)?, DeleteHostOutcome::AccountScoped);

            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM ingest_tokens WHERE host_id = ?1",
                    "hostA"
                ),
                0
            );
            assert_eq!(
                count(c, "SELECT COUNT(*) FROM hosts WHERE host_id = ?1", "hostA"),
                0
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM usage_buckets WHERE host_id = ?1",
                    "hostA"
                ),
                0
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM usage_sessions WHERE host_id = ?1",
                    "hostA"
                ),
                0
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM daily_rollups WHERE host_id = ?1",
                    "hostA"
                ),
                0
            );

            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM ingest_tokens WHERE host_id = ?1",
                    "hostB"
                ),
                1
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM usage_buckets WHERE host_id = ?1",
                    "hostB"
                ),
                1
            );
            assert_eq!(
                count(c, "SELECT COUNT(*) FROM hosts WHERE host_id = ?1", &acct),
                1
            );
            let acct_tokens: i64 = c.query_row(
                "SELECT input_tokens FROM usage_buckets WHERE host_id = ?1",
                params![acct],
                |r| r.get(0),
            )?;
            assert_eq!(acct_tokens, 100);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn expire_approved_unclaimed_revokes_token() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            insert_join(
                c,
                "jid",
                "a".repeat(64).as_str(),
                "1234",
                "box",
                None,
                "2026-01-01T00:00:00+00:00",
                "2026-01-01T00:10:00+00:00",
            )?;
            insert_token(c, "th", "hostZ", "aiu_zzzzzzzz", None, "box")?;
            assert!(approve_join(c, "jid", "aiu_secret", "hostZ")?);
            expire_stale_joins(c, "2026-01-01T00:11:00+00:00")?;
            let row = get_join(c, "jid")?.unwrap();
            assert_eq!(row.status, "expired");
            assert!(row.token.is_none());
            let tok = lookup_token(c, "th")?.unwrap();
            assert!(tok.revoked);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn claim_join_is_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            insert_join(
                c,
                "jid2",
                "b".repeat(64).as_str(),
                "9999",
                "box",
                None,
                "2026-01-01T00:00:00+00:00",
                "2026-01-01T00:10:00+00:00",
            )?;
            assert!(approve_join(c, "jid2", "aiu_once", "hostY")?);
            let first = claim_join(c, "jid2")?;
            assert_eq!(first, Some(("aiu_once".into(), "hostY".into())));
            assert!(claim_join(c, "jid2")?.is_none());
            Ok(())
        })
        .unwrap();
    }
}
