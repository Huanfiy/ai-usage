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
        migrate_session_tokens(&conn)?;
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
  agent_version TEXT
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
CREATE INDEX IF NOT EXISTS idx_buckets_start ON usage_buckets(bucket_start);
CREATE INDEX IF NOT EXISTS idx_buckets_host ON usage_buckets(host_id, bucket_start);
CREATE INDEX IF NOT EXISTS idx_rollups_day ON daily_rollups(day);
CREATE INDEX IF NOT EXISTS idx_sessions_last ON usage_sessions(last_message_at);
"#;

fn migrate_session_tokens(conn: &Connection) -> Result<()> {
    let existing: std::collections::HashSet<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(usage_sessions)")?;
        let names = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        names
    };
    for (name, sql) in [
        (
            "input_tokens",
            "ALTER TABLE usage_sessions ADD COLUMN input_tokens INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "output_tokens",
            "ALTER TABLE usage_sessions ADD COLUMN output_tokens INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "cache_read_input_tokens",
            "ALTER TABLE usage_sessions ADD COLUMN cache_read_input_tokens INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "cache_creation_input_tokens",
            "ALTER TABLE usage_sessions ADD COLUMN cache_creation_input_tokens INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "reasoning_output_tokens",
            "ALTER TABLE usage_sessions ADD COLUMN reasoning_output_tokens INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "total_tokens",
            "ALTER TABLE usage_sessions ADD COLUMN total_tokens INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        if !existing.contains(name) {
            conn.execute(sql, [])?;
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

pub fn token_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM ingest_tokens WHERE revoked_at IS NULL",
        [],
        |r| r.get(0),
    )?)
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
            IngestRequest {
                schema_version: 1,
                hostname: Some(hostname.into()),
                agent_version: Some("0.1.0".into()),
                buckets,
                sessions: vec![session()],
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
}
