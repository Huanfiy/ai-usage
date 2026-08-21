use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

const ACCESS_TOKEN_KEY: &str = "cursorAuth/accessToken";
const CACHED_EMAIL_KEY: &str = "cursorAuth/cachedEmail";

pub struct LocalAuth {
    pub access_token: String,
    pub cached_email: String,
}

/// Read-only URI open. Does not copy the WAL set (the live DB can be multi-GB).
pub fn read_auth(path: &Path) -> Result<Option<LocalAuth>, rusqlite::Error> {
    let conn = open_readonly(path)?;
    let token = read_item(&conn, ACCESS_TOKEN_KEY)?;
    let token = match token {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => return Ok(None),
    };
    let cached_email = read_item(&conn, CACHED_EMAIL_KEY)?
        .map(|s| strip_wrapping_quotes(s.trim()))
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    Ok(Some(LocalAuth {
        access_token: token,
        cached_email,
    }))
}

fn open_readonly(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        sqlite_ro_uri(path),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(Duration::from_secs(8))?;
    Ok(conn)
}

fn sqlite_ro_uri(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut uri = String::from("file:");
    for c in raw.chars() {
        match c {
            ' ' => uri.push_str("%20"),
            '#' => uri.push_str("%23"),
            '?' => uri.push_str("%3F"),
            _ => uri.push(c),
        }
    }
    uri.push_str("?mode=ro");
    uri
}

fn read_item(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    let value: Option<rusqlite::types::Value> = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match value {
        Some(rusqlite::types::Value::Text(s)) => Some(s),
        Some(rusqlite::types::Value::Blob(b)) => String::from_utf8(b).ok(),
        _ => None,
    })
}

fn strip_wrapping_quotes(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn write_items(path: &Path, items: &[(&str, &str)]) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT);")
            .unwrap();
        for (k, v) in items {
            conn.execute(
                "INSERT INTO ItemTable(key, value) VALUES(?1, ?2)",
                params![k, v],
            )
            .unwrap();
        }
    }

    #[test]
    fn reads_token_and_cached_email_only() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.vscdb");
        write_items(
            &db,
            &[
                (ACCESS_TOKEN_KEY, "header.payload.sig"),
                (CACHED_EMAIL_KEY, "\"you@example.com\""),
                ("unrelated", "nope"),
            ],
        );
        let auth = read_auth(&db).unwrap().unwrap();
        assert_eq!(auth.access_token, "header.payload.sig");
        assert_eq!(auth.cached_email, "you@example.com");
    }

    #[test]
    fn missing_token_is_logged_out() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.vscdb");
        write_items(&db, &[(CACHED_EMAIL_KEY, "you@example.com")]);
        assert!(read_auth(&db).unwrap().is_none());
    }
}
