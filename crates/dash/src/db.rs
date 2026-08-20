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

pub fn token_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM ingest_tokens WHERE revoked_at IS NULL",
        [],
        |r| r.get(0),
    )?)
}
