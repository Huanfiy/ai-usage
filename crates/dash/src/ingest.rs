use ai_usage_protocol::{
    account_host_id, is_account_scoped, is_known_source, is_valid_account_hash, normalize_timezone,
    utc_day, CursorAccountUsage, IngestRequest, IngestResponse, UsageBucket, UsageSession,
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
    timezone: Option<&str>,
    req: IngestRequest,
) -> Result<IngestResponse> {
    let now = Utc::now().to_rfc3339();
    let timezone = timezone.and_then(normalize_timezone);
    conn.execute(
        "INSERT INTO hosts(host_id, hostname, last_seen, agent_version, timezone)
         VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(host_id) DO UPDATE SET
           hostname=excluded.hostname,
           last_seen=excluded.last_seen,
           agent_version=COALESCE(excluded.agent_version, hosts.agent_version),
           timezone=COALESCE(excluded.timezone, hosts.timezone)",
        params![host_id, hostname, now, agent_version, timezone],
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
        let Some(hid) = bucket_host_id(host_id, &bucket) else {
            resp.dropped.buckets += 1;
            continue;
        };
        if is_account_scoped(&bucket.source) {
            upsert_account_host(&*tx, &hid, &bucket.account_label, &now)?;
        }
        match upsert_bucket(&*tx, &hid, &bucket, &now)? {
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
    for usage in req.cursor_accounts {
        let usage = usage.normalize();
        if !is_valid_account_hash(&usage.account_hash) {
            continue;
        }
        upsert_cursor_usage(&*tx, &usage, &now)?;
    }
    tx.commit()?;
    resp.dropped.unknown_sources = unknown;
    Ok(resp)
}

/// 账号套餐快照按 `fetched_at` 新者胜：多机上报同一账号自动收敛到最新一份。
fn upsert_cursor_usage(conn: &Connection, u: &CursorAccountUsage, now: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO cursor_account_usage(
            account_hash, account_label, membership, subscription_status, billing_cycle_end,
            api_percent, auto_percent, bot_percent, bot_period_start, bot_next_reset,
            bot_available, plan_used, plan_limit, included_cents, bonus_cents,
            auto_used, auto_limit, fetched_at, updated_at,
            credit_remaining_cents, credit_total_cents, credit_expires_at, credit_label)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,
                ?20,?21,?22,?23)
         ON CONFLICT(account_hash) DO UPDATE SET
            account_label=excluded.account_label,
            membership=excluded.membership,
            subscription_status=excluded.subscription_status,
            billing_cycle_end=excluded.billing_cycle_end,
            api_percent=excluded.api_percent,
            auto_percent=excluded.auto_percent,
            bot_percent=excluded.bot_percent,
            bot_period_start=excluded.bot_period_start,
            bot_next_reset=excluded.bot_next_reset,
            bot_available=excluded.bot_available,
            plan_used=excluded.plan_used,
            plan_limit=excluded.plan_limit,
            included_cents=excluded.included_cents,
            bonus_cents=excluded.bonus_cents,
            auto_used=excluded.auto_used,
            auto_limit=excluded.auto_limit,
            fetched_at=excluded.fetched_at,
            updated_at=excluded.updated_at,
            credit_remaining_cents=excluded.credit_remaining_cents,
            credit_total_cents=excluded.credit_total_cents,
            credit_expires_at=excluded.credit_expires_at,
            credit_label=excluded.credit_label
         WHERE excluded.fetched_at >= cursor_account_usage.fetched_at",
        params![
            u.account_hash,
            u.account_label,
            u.membership,
            u.subscription_status,
            u.billing_cycle_end,
            u.api_percent,
            u.auto_percent,
            u.bot_percent,
            u.bot_period_start,
            u.bot_next_reset,
            u.bot_available.map(|b| b as i64),
            u.plan_used,
            u.plan_limit,
            u.included_cents,
            u.bonus_cents,
            u.auto_used,
            u.auto_limit,
            u.fetched_at.to_rfc3339(),
            now,
            u.credit_remaining_cents,
            u.credit_total_cents,
            u.credit_expires_at,
            u.credit_label
        ],
    )?;
    Ok(())
}

fn bucket_host_id(machine_host_id: &str, bucket: &UsageBucket) -> Option<String> {
    if !is_account_scoped(&bucket.source) {
        return Some(machine_host_id.to_string());
    }
    if is_valid_account_hash(&bucket.account_hash) {
        Some(account_host_id(&bucket.account_hash))
    } else {
        None
    }
}

fn upsert_account_host(conn: &Connection, host_id: &str, label: &str, now: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO hosts(host_id, hostname, last_seen, agent_version)
         VALUES(?1, ?2, ?3, NULL)
         ON CONFLICT(host_id) DO UPDATE SET
           last_seen = excluded.last_seen,
           hostname = CASE
             WHEN excluded.hostname LIKE '%@%' THEN excluded.hostname
             WHEN hosts.hostname = '' THEN excluded.hostname
             ELSE hosts.hostname
           END",
        params![host_id, label, now],
    )?;
    Ok(())
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
            duration_seconds, active_seconds, message_count, user_message_count,
            input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens,
            reasoning_output_tokens, total_tokens, updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
         ON CONFLICT(host_id, source, session_hash) DO UPDATE SET
            project=excluded.project,
            first_message_at=excluded.first_message_at,
            last_message_at=excluded.last_message_at,
            duration_seconds=excluded.duration_seconds,
            active_seconds=excluded.active_seconds,
            message_count=excluded.message_count,
            user_message_count=excluded.user_message_count,
            input_tokens=excluded.input_tokens,
            output_tokens=excluded.output_tokens,
            cache_read_input_tokens=excluded.cache_read_input_tokens,
            cache_creation_input_tokens=excluded.cache_creation_input_tokens,
            reasoning_output_tokens=excluded.reasoning_output_tokens,
            total_tokens=excluded.total_tokens,
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
            s.input_tokens,
            s.output_tokens,
            s.cache_read_input_tokens,
            s.cache_creation_input_tokens,
            s.reasoning_output_tokens,
            s.total_tokens,
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
            account_hash: String::new(),
            account_label: String::new(),
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
                timezone: None,
                buckets: vec![sample_bucket(100)],
                sessions: vec![],
                cursor_accounts: vec![],
            };
            let r1 = ingest(c, "host1", "a", Some("0.1.0"), None, req)?;
            assert_eq!(r1.ingested, 1);
            let req = IngestRequest {
                schema_version: 1,
                hostname: Some("a".into()),
                agent_version: Some("0.1.0".into()),
                timezone: None,
                buckets: vec![sample_bucket(40)],
                sessions: vec![],
                cursor_accounts: vec![],
            };
            let r2 = ingest(c, "host1", "a", Some("0.1.0"), None, req)?;
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
            b.source = "not-a-source".into();
            let req = IngestRequest {
                schema_version: 1,
                hostname: Some("a".into()),
                agent_version: None,
                timezone: None,
                buckets: vec![b],
                sessions: vec![],
                cursor_accounts: vec![],
            };
            let r = ingest(c, "host1", "a", None, None, req)?;
            assert_eq!(r.dropped.buckets, 1);
            assert_eq!(r.dropped.unknown_sources, vec!["not-a-source".to_string()]);
            Ok(())
        })
        .unwrap();
    }

    fn cursor_bucket(hash: &str, label: &str, tokens: i64) -> UsageBucket {
        let ts = Utc.with_ymd_and_hms(2026, 1, 15, 10, 17, 0).unwrap();
        UsageBucket {
            source: ai_usage_protocol::SOURCE_CURSOR.into(),
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
            account_label: label.into(),
        }
    }

    fn hash_a() -> String {
        ai_usage_protocol::account_hash_from_sub("acct-alpha")
    }

    fn hash_b() -> String {
        ai_usage_protocol::account_hash_from_sub("acct-beta")
    }

    fn ingest_one(
        c: &Connection,
        host: &str,
        hostname: &str,
        buckets: Vec<UsageBucket>,
    ) -> IngestResponse {
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
                sessions: vec![],
                cursor_accounts: vec![],
            },
        )
        .unwrap()
    }

    #[test]
    fn same_account_from_two_machines_is_one_row() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            let h = hash_a();
            let r1 = ingest_one(c, "hostA", "pc1", vec![cursor_bucket(&h, "a@x.com", 100)]);
            let r2 = ingest_one(c, "hostB", "pc2", vec![cursor_bucket(&h, "a@x.com", 100)]);
            assert_eq!(r1.ingested, 1);
            assert_eq!(r2.ingested, 1);
            let n: i64 = c.query_row("SELECT COUNT(*) FROM usage_buckets", [], |r| r.get(0))?;
            let sum: i64 = c.query_row("SELECT SUM(input_tokens) FROM usage_buckets", [], |r| {
                r.get(0)
            })?;
            assert_eq!(n, 1);
            assert_eq!(sum, 100);
            let hid: String = c.query_row("SELECT host_id FROM usage_buckets", [], |r| r.get(0))?;
            assert_eq!(hid, ai_usage_protocol::account_host_id(&h));
            let hosts: i64 = c.query_row("SELECT COUNT(*) FROM hosts", [], |r| r.get(0))?;
            assert_eq!(hosts, 3);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn two_accounts_are_not_merged() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            ingest_one(
                c,
                "hostA",
                "pc1",
                vec![cursor_bucket(&hash_a(), "a@x.com", 10)],
            );
            ingest_one(
                c,
                "hostB",
                "pc2",
                vec![cursor_bucket(&hash_b(), "b@y.com", 20)],
            );
            let n: i64 = c.query_row("SELECT COUNT(*) FROM usage_buckets", [], |r| r.get(0))?;
            let sum: i64 = c.query_row("SELECT SUM(input_tokens) FROM usage_buckets", [], |r| {
                r.get(0)
            })?;
            assert_eq!(n, 2);
            assert_eq!(sum, 30);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn cursor_without_valid_hash_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            let cases = [
                "",
                "abc",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "gggggggggggggggggggggggggggggggg",
            ];
            for hash in cases {
                let r = ingest_one(c, "host1", "a", vec![cursor_bucket(hash, "a@x.com", 5)]);
                assert_eq!(r.dropped.buckets, 1, "hash={hash:?}");
                assert!(r.dropped.unknown_sources.is_empty());
            }
            let n: i64 = c.query_row("SELECT COUNT(*) FROM usage_buckets", [], |r| r.get(0))?;
            assert_eq!(n, 0);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn machine_source_ignores_account_hash() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            let mut b = sample_bucket(7);
            b.account_hash = hash_a();
            b.account_label = "a@x.com".into();
            ingest_one(c, "host1", "a", vec![b]);
            let hid: String = c.query_row("SELECT host_id FROM usage_buckets", [], |r| r.get(0))?;
            assert_eq!(hid, "host1");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn account_live_window_keeps_larger_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            let h = hash_a();
            assert_eq!(
                ingest_one(c, "hostA", "pc1", vec![cursor_bucket(&h, "a@x.com", 100)]).ingested,
                1
            );
            let r = ingest_one(c, "hostB", "pc2", vec![cursor_bucket(&h, "a@x.com", 40)]);
            assert_eq!(r.protected.buckets, 1);
            let n: i64 = c.query_row("SELECT input_tokens FROM usage_buckets", [], |r| r.get(0))?;
            assert_eq!(n, 100);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn cursor_account_usage_latest_fetched_at_wins() {
        use ai_usage_protocol::CursorAccountUsage;
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            let h = hash_a();
            let mk = |pct: f64, fetched: &str| CursorAccountUsage {
                account_hash: h.clone(),
                account_label: "a@x.com".into(),
                api_percent: Some(pct),
                bot_percent: Some(0.5),
                bot_available: Some(true),
                credit_remaining_cents: Some(8415),
                credit_total_cents: Some(10000),
                credit_expires_at: Some("2026-09-03T19:06:49Z".into()),
                credit_label: Some("Cursor Grok 4.6 Credit".into()),
                fetched_at: chrono::DateTime::parse_from_rfc3339(fetched)
                    .unwrap()
                    .with_timezone(&Utc),
                ..CursorAccountUsage::default()
            };
            let send = |c: &Connection, usage: CursorAccountUsage| {
                ingest(
                    c,
                    "hostA",
                    "pc1",
                    Some("0.1.0"),
                    None,
                    IngestRequest {
                        schema_version: 1,
                        hostname: Some("pc1".into()),
                        agent_version: Some("0.1.0".into()),
                        timezone: None,
                        buckets: vec![],
                        sessions: vec![],
                        cursor_accounts: vec![usage],
                    },
                )
                .unwrap()
            };
            send(c, mk(10.0, "2026-08-29T10:00:00Z"));
            // 更旧的快照（另一台机器时钟滞后）不覆盖
            send(c, mk(5.0, "2026-08-29T09:00:00Z"));
            let pct: f64 = c.query_row(
                "SELECT api_percent FROM cursor_account_usage WHERE account_hash=?1",
                params![h],
                |r| r.get(0),
            )?;
            assert_eq!(pct, 10.0);
            // 更新的快照覆盖
            send(c, mk(20.0, "2026-08-29T11:00:00Z"));
            let pct: f64 = c.query_row(
                "SELECT api_percent FROM cursor_account_usage WHERE account_hash=?1",
                params![h],
                |r| r.get(0),
            )?;
            assert_eq!(pct, 20.0);
            let n: i64 =
                c.query_row("SELECT COUNT(*) FROM cursor_account_usage", [], |r| r.get(0))?;
            assert_eq!(n, 1);
            // 非法 hash 丢弃
            let mut bad = mk(1.0, "2026-08-29T12:00:00Z");
            bad.account_hash = "not-hex".into();
            send(c, bad);
            let n: i64 =
                c.query_row("SELECT COUNT(*) FROM cursor_account_usage", [], |r| r.get(0))?;
            assert_eq!(n, 1);
            let rows = crate::db::list_cursor_accounts(c)?;
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["bot_available"], true);
            assert_eq!(rows[0]["bot_percent"], 0.5);
            assert_eq!(rows[0]["credit_remaining_cents"], 8415);
            assert_eq!(rows[0]["credit_total_cents"], 10000);
            assert_eq!(rows[0]["credit_expires_at"], "2026-09-03T19:06:49Z");
            assert_eq!(rows[0]["credit_label"], "Cursor Grok 4.6 Credit");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn account_label_email_wins_over_short_hash() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            let h = hash_a();
            let hid = ai_usage_protocol::account_host_id(&h);
            ingest_one(c, "hostA", "pc1", vec![cursor_bucket(&h, "a@x.com", 1)]);
            ingest_one(c, "hostB", "pc2", vec![cursor_bucket(&h, &h[..8], 1)]);
            let name: String = c.query_row(
                "SELECT hostname FROM hosts WHERE host_id=?1",
                params![hid],
                |r| r.get(0),
            )?;
            assert_eq!(name, "a@x.com");

            let h2 = hash_b();
            let hid2 = ai_usage_protocol::account_host_id(&h2);
            ingest_one(c, "hostA", "pc1", vec![cursor_bucket(&h2, &h2[..8], 1)]);
            ingest_one(c, "hostB", "pc2", vec![cursor_bucket(&h2, "b@y.com", 1)]);
            let name2: String = c.query_row(
                "SELECT hostname FROM hosts WHERE host_id=?1",
                params![hid2],
                |r| r.get(0),
            )?;
            assert_eq!(name2, "b@y.com");
            Ok(())
        })
        .unwrap();
    }
}
