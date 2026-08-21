use ai_usage_protocol::{
    is_known_source, utc_day, IngestRequest, IngestResponse, UsageBucket, UsageSession,
};
use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, Default)]
struct OldBucket {
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    reasoning_output_tokens: i64,
    total_tokens: i64,
}

impl OldBucket {
    fn score(&self) -> i64 {
        self.input_tokens
            + self.output_tokens
            + self.cache_read_input_tokens
            + self.cache_creation_input_tokens
            + self.reasoning_output_tokens
    }
}

pub fn ingest(
    conn: &Connection,
    host_id: &str,
    hostname: &str,
    agent_version: Option<&str>,
    req: IngestRequest,
) -> Result<IngestResponse> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO hosts(host_id, hostname, last_seen, agent_version) VALUES(?1,?2,?3,?4)
         ON CONFLICT(host_id) DO UPDATE SET
           hostname=excluded.hostname,
           last_seen=excluded.last_seen,
           agent_version=COALESCE(excluded.agent_version, hosts.agent_version)",
        params![host_id, hostname, now, agent_version],
    )?;

    let mut resp = IngestResponse::default();
    let mut unknown = Vec::new();
    let tx = conn.unchecked_transaction()?;

    for bucket in req.buckets {
        let bucket = bucket.normalize();
        if !is_known_source(&bucket.source) {
            resp.dropped.buckets += 1;
            if !unknown.contains(&bucket.source) {
                unknown.push(bucket.source);
            }
            continue;
        }
        match upsert_bucket(&*tx, host_id, &bucket, &now)? {
            Upsert::Inserted | Upsert::Replaced => resp.ingested += 1,
            Upsert::Protected => resp.protected.buckets += 1,
        }
    }
    for session in req.sessions {
        let session = session.normalize();
        if !is_known_source(&session.source) {
            continue;
        }
        upsert_session(&*tx, host_id, &session, &now)?;
        resp.sessions += 1;
    }
    tx.commit()?;
    resp.dropped.unknown_sources = unknown;
    Ok(resp)
}

enum Upsert {
    Inserted,
    Replaced,
    Protected,
}

fn upsert_bucket(conn: &Connection, host_id: &str, b: &UsageBucket, now: &str) -> Result<Upsert> {
    let start = b.bucket_start.to_rfc3339();
    let old: Option<OldBucket> = conn
        .query_row(
            "SELECT input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens,
                    reasoning_output_tokens, total_tokens
             FROM usage_buckets
             WHERE host_id=?1 AND source=?2 AND model=?3 AND project=?4 AND bucket_start=?5",
            params![host_id, b.source, b.model, b.project, start],
            |r| {
                Ok(OldBucket {
                    input_tokens: r.get(0)?,
                    output_tokens: r.get(1)?,
                    cache_read_input_tokens: r.get(2)?,
                    cache_creation_input_tokens: r.get(3)?,
                    reasoning_output_tokens: r.get(4)?,
                    total_tokens: r.get(5)?,
                })
            },
        )
        .optional()?;

    if let Some(old) = &old {
        if old.score() > b.token_score() {
            return Ok(Upsert::Protected);
        }
        apply_rollup(conn, host_id, b, -1, old)?;
    }

    conn.execute(
        "INSERT INTO usage_buckets(
            host_id, source, model, project, bucket_start,
            input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens,
            reasoning_output_tokens, total_tokens, updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(host_id, source, model, project, bucket_start) DO UPDATE SET
            input_tokens=excluded.input_tokens,
            output_tokens=excluded.output_tokens,
            cache_read_input_tokens=excluded.cache_read_input_tokens,
            cache_creation_input_tokens=excluded.cache_creation_input_tokens,
            reasoning_output_tokens=excluded.reasoning_output_tokens,
            total_tokens=excluded.total_tokens,
            updated_at=excluded.updated_at",
        params![
            host_id,
            b.source,
            b.model,
            b.project,
            start,
            b.input_tokens,
            b.output_tokens,
            b.cache_read_input_tokens,
            b.cache_creation_input_tokens,
            b.reasoning_output_tokens,
            b.total_tokens,
            now
        ],
    )?;
    let fresh = OldBucket {
        input_tokens: b.input_tokens,
        output_tokens: b.output_tokens,
        cache_read_input_tokens: b.cache_read_input_tokens,
        cache_creation_input_tokens: b.cache_creation_input_tokens,
        reasoning_output_tokens: b.reasoning_output_tokens,
        total_tokens: b.total_tokens,
    };
    apply_rollup(conn, host_id, b, 1, &fresh)?;
    Ok(if old.is_some() {
        Upsert::Replaced
    } else {
        Upsert::Inserted
    })
}

fn apply_rollup(
    conn: &Connection,
    host_id: &str,
    b: &UsageBucket,
    sign: i64,
    tokens: &OldBucket,
) -> Result<()> {
    let day = utc_day(b.bucket_start);
    conn.execute(
        "INSERT INTO daily_rollups(day, host_id, source, model, project,
            input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens,
            reasoning_output_tokens, total_tokens)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(day, host_id, source, model, project) DO UPDATE SET
            input_tokens = daily_rollups.input_tokens + excluded.input_tokens,
            output_tokens = daily_rollups.output_tokens + excluded.output_tokens,
            cache_read_input_tokens = daily_rollups.cache_read_input_tokens + excluded.cache_read_input_tokens,
            cache_creation_input_tokens = daily_rollups.cache_creation_input_tokens + excluded.cache_creation_input_tokens,
            reasoning_output_tokens = daily_rollups.reasoning_output_tokens + excluded.reasoning_output_tokens,
            total_tokens = daily_rollups.total_tokens + excluded.total_tokens",
        params![
            day,
            host_id,
            b.source,
            b.model,
            b.project,
            sign * tokens.input_tokens,
            sign * tokens.output_tokens,
            sign * tokens.cache_read_input_tokens,
            sign * tokens.cache_creation_input_tokens,
            sign * tokens.reasoning_output_tokens,
            sign * tokens.total_tokens
        ],
    )?;
    Ok(())
}

fn upsert_session(conn: &Connection, host_id: &str, s: &UsageSession, now: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO usage_sessions(
            host_id, source, session_hash, project, first_message_at, last_message_at,
            duration_seconds, active_seconds, message_count, user_message_count, updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(host_id, source, session_hash) DO UPDATE SET
            project=excluded.project,
            first_message_at=excluded.first_message_at,
            last_message_at=excluded.last_message_at,
            duration_seconds=excluded.duration_seconds,
            active_seconds=excluded.active_seconds,
            message_count=excluded.message_count,
            user_message_count=excluded.user_message_count,
            updated_at=excluded.updated_at",
        params![
            host_id,
            s.source,
            s.session_hash,
            s.project,
            s.first_message_at.to_rfc3339(),
            s.last_message_at.to_rfc3339(),
            s.duration_seconds,
            s.active_seconds,
            s.message_count,
            s.user_message_count,
            now
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use ai_usage_protocol::round_to_half_hour;
    use chrono::TimeZone;

    fn sample_bucket(tokens: i64) -> UsageBucket {
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
        }
    }

    #[test]
    fn live_window_keeps_larger_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            let req = IngestRequest {
                schema_version: 1,
                hostname: Some("a".into()),
                agent_version: Some("0.1.0".into()),
                buckets: vec![sample_bucket(100)],
                sessions: vec![],
            };
            let r1 = ingest(c, "host1", "a", Some("0.1.0"), req)?;
            assert_eq!(r1.ingested, 1);
            let req = IngestRequest {
                schema_version: 1,
                hostname: Some("a".into()),
                agent_version: Some("0.1.0".into()),
                buckets: vec![sample_bucket(40)],
                sessions: vec![],
            };
            let r2 = ingest(c, "host1", "a", Some("0.1.0"), req)?;
            assert_eq!(r2.protected.buckets, 1);
            let n: i64 = c.query_row("SELECT input_tokens FROM usage_buckets", [], |r| r.get(0))?;
            assert_eq!(n, 100);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn unknown_source_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            let mut b = sample_bucket(10);
            b.source = "cursor".into();
            let req = IngestRequest {
                schema_version: 1,
                hostname: Some("a".into()),
                agent_version: None,
                buckets: vec![b],
                sessions: vec![],
            };
            let r = ingest(c, "host1", "a", None, req)?;
            assert_eq!(r.dropped.buckets, 1);
            assert_eq!(r.dropped.unknown_sources, vec!["cursor".to_string()]);
            Ok(())
        })
        .unwrap();
    }
}
