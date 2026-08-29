use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use rusqlite::{params_from_iter, Connection};
use serde::Serialize;

use crate::model::display_model;
use crate::pricing::{ModelPrice, PriceBook, TokenSlice};

#[derive(Debug, Clone)]
pub struct QueryFilter {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub host_id: Option<String>,
    pub sources: Vec<String>,
    pub models: Vec<String>,
    pub projects: Vec<String>,
    pub hide_projects: bool,
}

impl QueryFilter {
    pub fn from_params(
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        host: Option<String>,
        source: Option<String>,
        model: Option<String>,
        project: Option<String>,
        hide_projects: bool,
    ) -> Self {
        let to = to.unwrap_or_else(Utc::now);
        let from = from.unwrap_or(to - Duration::days(7));
        Self {
            from,
            to,
            host_id: host.filter(|s| !s.is_empty() && s != "all"),
            sources: split_csv(source),
            models: split_csv(model),
            projects: split_csv(project),
            hide_projects,
        }
    }

    fn use_rollups(&self) -> bool {
        (self.to - self.from) > Duration::days(2)
    }

    /// Half-open `[from, to)` as UTC calendar days for `daily_rollups`.
    ///
    /// A mid-day `to` must include that UTC date: `day < to.date` would drop
    /// today when the UI 7D window ends at now.
    fn rollup_day_bounds(&self) -> (String, String) {
        (
            self.from.date_naive().to_string(),
            exclusive_rollup_end(self.to).to_string(),
        )
    }
}

fn exclusive_rollup_end(to: DateTime<Utc>) -> chrono::NaiveDate {
    let d = to.date_naive();
    if to.time() == chrono::NaiveTime::MIN {
        d
    } else {
        d.succ_opt().unwrap_or(d)
    }
}

fn split_csv(v: Option<String>) -> Vec<String> {
    v.unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TokenTotals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub reasoning: i64,
    pub total: i64,
}

impl TokenTotals {
    fn add_row(&mut self, r: &TokenRow) {
        self.input += r.input;
        self.output += r.output;
        self.cache_read += r.cache_read;
        self.cache_creation += r.cache_write;
        self.reasoning += r.reasoning;
        self.total += r.total;
    }
}

#[derive(Debug, Clone)]
struct TokenRow {
    host_id: String,
    source: String,
    /// Folded display name for grouping and filters.
    model: String,
    /// Raw ingested slug; pricing reads this only.
    billed_model: String,
    project: String,
    bucket_start: String,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
    total: i64,
}

fn scan_rows(conn: &Connection, f: &QueryFilter) -> Result<Vec<TokenRow>> {
    scan_table(conn, f, f.use_rollups())
}

fn scan_table(conn: &Connection, f: &QueryFilter, use_rollups: bool) -> Result<Vec<TokenRow>> {
    let (table, time_col) = if use_rollups {
        ("daily_rollups", "day")
    } else {
        ("usage_buckets", "bucket_start")
    };
    let mut sql = format!(
        "SELECT host_id, source, model, project, {time_col},
                input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens,
                reasoning_output_tokens, total_tokens
         FROM {table} WHERE {time_col} >= ? AND {time_col} < ?"
    );
    let mut params: Vec<String> = if use_rollups {
        let (a, b) = f.rollup_day_bounds();
        vec![a, b]
    } else {
        vec![f.from.to_rfc3339(), f.to.to_rfc3339()]
    };
    if let Some(host) = &f.host_id {
        sql.push_str(" AND host_id = ?");
        params.push(host.clone());
    }
    push_in(&mut sql, &mut params, "source", &f.sources);
    if !f.hide_projects {
        push_in(&mut sql, &mut params, "project", &f.projects);
    }
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt
        .query_map(params_from_iter(params.iter()), |r| {
            let billed_model: String = r.get(2)?;
            Ok(TokenRow {
                host_id: r.get(0)?,
                source: r.get(1)?,
                model: display_model(&billed_model),
                billed_model,
                project: if f.hide_projects {
                    "unknown".into()
                } else {
                    r.get(3)?
                },
                bucket_start: r.get(4)?,
                input: r.get(5)?,
                output: r.get(6)?,
                cache_read: r.get(7)?,
                cache_write: r.get(8)?,
                reasoning: r.get(9)?,
                total: r.get(10)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !f.models.is_empty() {
        let wanted: std::collections::HashSet<String> = f
            .models
            .iter()
            .map(|m| display_model(m).to_ascii_lowercase())
            .collect();
        rows.retain(|r| wanted.contains(&r.model.to_ascii_lowercase()));
    }
    Ok(rows)
}

fn token_slice(r: &TokenRow) -> TokenSlice {
    TokenSlice {
        input: r.input,
        output: r.output,
        cache_read: r.cache_read,
        cache_write: r.cache_write,
        reasoning: r.reasoning,
    }
}

fn push_in(sql: &mut String, params: &mut Vec<String>, col: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    sql.push_str(" AND ");
    sql.push_str(col);
    sql.push_str(" IN (");
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            sql.push(',');
        }
        sql.push('?');
        params.push(v.clone());
    }
    sql.push(')');
}

#[derive(Serialize)]
pub struct Summary {
    pub from: String,
    pub to: String,
    pub tokens: TokenTotals,
    pub cost_usd: f64,
    pub cost_coverage: f64,
    pub cache_hit_rate: f64,
    pub sessions: i64,
    pub message_count: i64,
    pub user_message_count: i64,
    pub duration_seconds: i64,
    pub active_seconds: i64,
    pub hosts: i64,
    pub sources: i64,
}

pub fn summary(conn: &Connection, book: &PriceBook, f: &QueryFilter) -> Result<Summary> {
    let rows = scan_rows(conn, f)?;
    let mut tokens = TokenTotals::default();
    let mut priced = 0i64;
    let mut cost = 0.0;
    let mut hosts = std::collections::HashSet::new();
    let mut sources = std::collections::HashSet::new();
    for r in &rows {
        tokens.add_row(r);
        hosts.insert(r.host_id.clone());
        sources.insert(r.source.clone());
        if let Some(c) = book.cost_usd(&r.billed_model, token_slice(r)) {
            cost += c;
            priced += r.total;
        }
    }
    let denom = tokens.input + tokens.cache_read + tokens.cache_creation;
    let cache_hit_rate = if denom > 0 {
        tokens.cache_read as f64 / denom as f64
    } else {
        0.0
    };
    let sess = session_totals(conn, f)?;
    Ok(Summary {
        from: f.from.to_rfc3339(),
        to: f.to.to_rfc3339(),
        cost_coverage: if tokens.total > 0 {
            priced as f64 / tokens.total as f64
        } else {
            1.0
        },
        tokens,
        cost_usd: cost,
        cache_hit_rate,
        sessions: sess.sessions,
        message_count: sess.message_count,
        user_message_count: sess.user_message_count,
        duration_seconds: sess.duration_seconds,
        active_seconds: sess.active_seconds,
        hosts: hosts.len() as i64,
        sources: sources.len() as i64,
    })
}

struct SessionTotals {
    sessions: i64,
    message_count: i64,
    user_message_count: i64,
    duration_seconds: i64,
    active_seconds: i64,
}

fn session_totals(conn: &Connection, f: &QueryFilter) -> Result<SessionTotals> {
    let mut sql = String::from(
        "SELECT COUNT(*), COALESCE(SUM(message_count),0), COALESCE(SUM(user_message_count),0),
                COALESCE(SUM(duration_seconds),0), COALESCE(SUM(active_seconds),0)
         FROM usage_sessions WHERE last_message_at >= ? AND last_message_at < ?",
    );
    let mut params = vec![f.from.to_rfc3339(), f.to.to_rfc3339()];
    if let Some(host) = &f.host_id {
        sql.push_str(" AND host_id = ?");
        params.push(host.clone());
    }
    push_in(&mut sql, &mut params, "source", &f.sources);
    if !f.hide_projects {
        push_in(&mut sql, &mut params, "project", &f.projects);
    }
    let row = conn.query_row(&sql, params_from_iter(params.iter()), |r| {
        Ok(SessionTotals {
            sessions: r.get(0)?,
            message_count: r.get(1)?,
            user_message_count: r.get(2)?,
            duration_seconds: r.get(3)?,
            active_seconds: r.get(4)?,
        })
    })?;
    Ok(row)
}

#[derive(Serialize)]
pub struct SeriesPoint {
    pub t: String,
    pub tokens: i64,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub cost_usd: f64,
}

pub fn series(conn: &Connection, book: &PriceBook, f: &QueryFilter) -> Result<Vec<SeriesPoint>> {
    let rows = scan_rows(conn, f)?;
    let mut map: std::collections::BTreeMap<String, SeriesPoint> =
        std::collections::BTreeMap::new();
    for r in rows {
        let cost = book
            .cost_usd(&r.billed_model, token_slice(&r))
            .unwrap_or(0.0);
        let e = map
            .entry(r.bucket_start.clone())
            .or_insert_with(|| SeriesPoint {
                t: r.bucket_start.clone(),
                tokens: 0,
                input: 0,
                output: 0,
                cache_read: 0,
                cache_creation: 0,
                cost_usd: 0.0,
            });
        e.tokens += r.total;
        e.input += r.input;
        e.output += r.output;
        e.cache_read += r.cache_read;
        e.cache_creation += r.cache_write;
        e.cost_usd += cost;
    }
    Ok(map.into_values().collect())
}

#[derive(Serialize)]
pub struct BreakdownItem {
    pub key: String,
    pub tokens: i64,
    pub cost_usd: f64,
    pub share: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPrice>,
}

pub fn breakdown(
    conn: &Connection,
    book: &PriceBook,
    f: &QueryFilter,
    by: &str,
) -> Result<Vec<BreakdownItem>> {
    let rows = scan_rows(conn, f)?;
    let by_model = by == "model";
    Ok(group_items(
        &rows,
        book,
        |r| match by {
            "model" => r.model.clone(),
            "project" => r.project.clone(),
            "host" => r.host_id.clone(),
            _ => r.source.clone(),
        },
        by_model,
    ))
}

struct GroupAcc {
    tokens: i64,
    cost_usd: f64,
    billed: Vec<String>,
}

fn group_items(
    rows: &[TokenRow],
    book: &PriceBook,
    key_of: impl Fn(&TokenRow) -> String,
    attach_pricing: bool,
) -> Vec<BreakdownItem> {
    let mut map: std::collections::BTreeMap<String, GroupAcc> = std::collections::BTreeMap::new();
    let mut total = 0i64;
    for r in rows {
        let cost = book
            .cost_usd(&r.billed_model, token_slice(r))
            .unwrap_or(0.0);
        let e = map.entry(key_of(r)).or_insert(GroupAcc {
            tokens: 0,
            cost_usd: 0.0,
            billed: Vec::new(),
        });
        e.tokens += r.total;
        e.cost_usd += cost;
        if attach_pricing && !e.billed.iter().any(|m| m == &r.billed_model) {
            e.billed.push(r.billed_model.clone());
        }
        total += r.total;
    }
    let mut items: Vec<_> = map
        .into_iter()
        .map(|(key, acc)| BreakdownItem {
            key,
            tokens: acc.tokens,
            cost_usd: acc.cost_usd,
            share: if total > 0 {
                acc.tokens as f64 / total as f64
            } else {
                0.0
            },
            pricing: if attach_pricing {
                consensus_price(book, &acc.billed)
            } else {
                None
            },
        })
        .collect();
    items.sort_by(|a, b| b.tokens.cmp(&a.tokens));
    items
}

fn consensus_price(book: &PriceBook, billed: &[String]) -> Option<ModelPrice> {
    let mut found: Option<ModelPrice> = None;
    for m in billed {
        let Some(p) = book.lookup(m) else {
            continue;
        };
        match &found {
            None => found = Some(p.clone()),
            Some(prev) if prev == p => {}
            Some(_) => return None,
        }
    }
    found
}

#[derive(Serialize)]
pub struct Distributions {
    pub host: Vec<BreakdownItem>,
    pub source: Vec<BreakdownItem>,
    pub model: Vec<BreakdownItem>,
    pub project: Vec<BreakdownItem>,
}

pub fn distributions(
    conn: &Connection,
    book: &PriceBook,
    f: &QueryFilter,
) -> Result<Distributions> {
    let rows = scan_rows(conn, f)?;
    Ok(Distributions {
        host: group_items(&rows, book, |r| r.host_id.clone(), false),
        source: group_items(&rows, book, |r| r.source.clone(), false),
        model: group_items(&rows, book, |r| r.model.clone(), true),
        project: group_items(&rows, book, |r| r.project.clone(), false),
    })
}

#[derive(Serialize)]
pub struct ActivityCell {
    pub dow: u8,
    pub hour: u8,
    pub tokens: i64,
    pub cost_usd: f64,
}

#[derive(Serialize)]
pub struct Activity {
    pub cells: Vec<ActivityCell>,
}

pub fn activity(conn: &Connection, book: &PriceBook, f: &QueryFilter) -> Result<Activity> {
    let rows = scan_table(conn, f, false)?;
    let mut cells = vec![(0i64, 0.0f64); 168];
    for r in rows {
        let Ok(dt) = DateTime::parse_from_rfc3339(&r.bucket_start) else {
            continue;
        };
        let utc = dt.with_timezone(&Utc);
        let dow = utc.weekday().num_days_from_sunday() as u8;
        let hour = utc.hour() as u8;
        let idx = (dow as usize) * 24 + (hour as usize);
        let cost = book
            .cost_usd(&r.billed_model, token_slice(&r))
            .unwrap_or(0.0);
        cells[idx].0 += r.total;
        cells[idx].1 += cost;
    }
    Ok(Activity {
        cells: (0..168)
            .map(|i| ActivityCell {
                dow: (i / 24) as u8,
                hour: (i % 24) as u8,
                tokens: cells[i].0,
                cost_usd: cells[i].1,
            })
            .collect(),
    })
}

#[derive(Serialize)]
pub struct SessionRow {
    pub host_id: String,
    pub source: String,
    pub project: String,
    pub session_hash: String,
    pub first_message_at: String,
    pub last_message_at: String,
    pub duration_seconds: i64,
    pub active_seconds: i64,
    pub message_count: i64,
    pub user_message_count: i64,
    pub tokens: TokenTotals,
}

pub fn sessions(conn: &Connection, f: &QueryFilter, limit: i64) -> Result<Vec<SessionRow>> {
    let mut sql = String::from(
        "SELECT host_id, source, project, session_hash, first_message_at, last_message_at,
                duration_seconds, active_seconds, message_count, user_message_count,
                input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens,
                reasoning_output_tokens, total_tokens
         FROM usage_sessions
         WHERE last_message_at >= ? AND last_message_at < ?",
    );
    let mut params = vec![f.from.to_rfc3339(), f.to.to_rfc3339()];
    if let Some(host) = &f.host_id {
        sql.push_str(" AND host_id = ?");
        params.push(host.clone());
    }
    push_in(&mut sql, &mut params, "source", &f.sources);
    if !f.hide_projects {
        push_in(&mut sql, &mut params, "project", &f.projects);
    }
    sql.push_str(" ORDER BY last_message_at DESC LIMIT ?");
    params.push(limit.to_string());
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(params.iter()), |r| {
            Ok(SessionRow {
                host_id: r.get(0)?,
                source: r.get(1)?,
                project: if f.hide_projects {
                    "unknown".into()
                } else {
                    r.get(2)?
                },
                session_hash: r.get(3)?,
                first_message_at: r.get(4)?,
                last_message_at: r.get(5)?,
                duration_seconds: r.get(6)?,
                active_seconds: r.get(7)?,
                message_count: r.get(8)?,
                user_message_count: r.get(9)?,
                tokens: TokenTotals {
                    input: r.get(10)?,
                    output: r.get(11)?,
                    cache_read: r.get(12)?,
                    cache_creation: r.get(13)?,
                    reasoning: r.get(14)?,
                    total: r.get(15)?,
                },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[derive(Serialize)]
pub struct HostRow {
    pub host_id: String,
    pub hostname: String,
    pub last_seen: String,
    pub agent_version: Option<String>,
    pub timezone: Option<String>,
}

pub fn hosts(conn: &Connection) -> Result<Vec<HostRow>> {
    let mut stmt = conn.prepare(
        "SELECT host_id, hostname, last_seen, agent_version, timezone FROM hosts ORDER BY last_seen DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(HostRow {
                host_id: r.get(0)?,
                hostname: r.get(1)?,
                last_seen: r.get(2)?,
                agent_version: r.get(3)?,
                timezone: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[derive(Serialize)]
pub struct FilterOptions {
    pub sources: Vec<String>,
    pub models: Vec<String>,
    pub projects: Vec<String>,
}

pub fn filter_options(conn: &Connection, f: &QueryFilter) -> Result<FilterOptions> {
    let rows = scan_rows(conn, f)?;
    let mut sources = std::collections::BTreeSet::new();
    let mut models = std::collections::BTreeSet::new();
    let mut projects = std::collections::BTreeSet::new();
    for r in rows {
        sources.insert(r.source);
        models.insert(r.model);
        if !f.hide_projects {
            projects.insert(r.project);
        }
    }
    Ok(FilterOptions {
        sources: sources.into_iter().collect(),
        models: models.into_iter().collect(),
        projects: projects.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use ai_usage_protocol::{round_to_half_hour, IngestRequest, UsageBucket, UsageSession};
    use chrono::TimeZone;

    use crate::db::Db;
    use crate::ingest::ingest;

    fn book() -> PriceBook {
        PriceBook::load(Path::new("/tmp/does-not-exist-ai-usage"), None).unwrap()
    }

    #[test]
    fn rollup_window_ending_mid_day_includes_that_day() {
        let midnight = Utc.with_ymd_and_hms(2026, 8, 21, 0, 0, 0).unwrap();
        let mid = Utc.with_ymd_and_hms(2026, 8, 21, 9, 47, 0).unwrap();
        let from = midnight - Duration::days(7);
        let open =
            QueryFilter::from_params(Some(from), Some(midnight), None, None, None, None, false);
        let live = QueryFilter::from_params(Some(from), Some(mid), None, None, None, None, false);
        assert!(open.use_rollups() && live.use_rollups());
        assert_eq!(open.rollup_day_bounds().1, "2026-08-21");
        assert_eq!(live.rollup_day_bounds().1, "2026-08-22");
    }

    fn window() -> QueryFilter {
        QueryFilter::from_params(
            Some(Utc.with_ymd_and_hms(2026, 1, 14, 0, 0, 0).unwrap()),
            Some(Utc.with_ymd_and_hms(2026, 1, 16, 0, 0, 0).unwrap()),
            None,
            None,
            None,
            None,
            false,
        )
    }

    fn sample_bucket(
        hour: u32,
        input: i64,
        output: i64,
        cache_read: i64,
        cache_write: i64,
    ) -> UsageBucket {
        let ts = Utc.with_ymd_and_hms(2026, 1, 15, hour, 17, 0).unwrap();
        UsageBucket {
            source: "codex".into(),
            model: "gpt-5.4".into(),
            project: "demo".into(),
            bucket_start: round_to_half_hour(ts),
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_write,
            reasoning_output_tokens: 0,
            total_tokens: input + output + cache_read + cache_write,
            account_hash: String::new(),
            account_label: String::new(),
        }
    }

    fn sample_session(
        hash: &str,
        messages: i64,
        user: i64,
        duration: i64,
        active: i64,
    ) -> UsageSession {
        let at = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        UsageSession {
            source: "codex".into(),
            project: "demo".into(),
            session_hash: hash.into(),
            first_message_at: at,
            last_message_at: at,
            duration_seconds: duration,
            active_seconds: active,
            message_count: messages,
            user_message_count: user,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 0,
        }
    }

    fn seed(c: &Connection, buckets: Vec<UsageBucket>, sessions: Vec<UsageSession>) {
        let req = IngestRequest {
            schema_version: 1,
            hostname: Some("a".into()),
            agent_version: Some("0.1.0".into()),
            timezone: None,
            buckets,
            sessions,
            cursor_accounts: vec![],
        };
        ingest(c, "host1", "a", Some("0.1.0"), None, req).unwrap();
    }

    #[test]
    fn summary_sums_session_messages_and_duration() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            seed(
                c,
                vec![],
                vec![
                    sample_session("s1", 10, 3, 100, 40),
                    sample_session("s2", 5, 2, 50, 20),
                    {
                        let mut outside = sample_session("s3", 99, 99, 999, 999);
                        outside.last_message_at =
                            Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap();
                        outside
                    },
                ],
            );
            let s = summary(c, &book(), &window())?;
            assert_eq!(s.sessions, 2);
            assert_eq!(s.message_count, 15);
            assert_eq!(s.user_message_count, 5);
            assert_eq!(s.duration_seconds, 150);
            assert_eq!(s.active_seconds, 60);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn summary_session_totals_ignore_model_filter() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            seed(c, vec![], vec![sample_session("s1", 10, 3, 100, 40)]);
            let mut f = window();
            f.models = vec!["does-not-exist".into()];
            let s = summary(c, &book(), &f)?;
            assert_eq!(s.sessions, 1);
            assert_eq!(s.message_count, 10);
            assert_eq!(s.user_message_count, 3);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn sessions_include_token_totals() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            let mut s = sample_session("s1", 10, 3, 100, 40);
            s.input_tokens = 12;
            s.output_tokens = 5;
            s.cache_read_input_tokens = 3;
            seed(c, vec![], vec![s]);
            let rows = sessions(c, &window(), 10)?;
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].tokens.input, 12);
            assert_eq!(rows[0].tokens.output, 5);
            assert_eq!(rows[0].tokens.cache_read, 3);
            assert_eq!(rows[0].tokens.total, 20);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn series_points_include_token_parts() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            seed(c, vec![sample_bucket(10, 100, 20, 30, 10)], vec![]);
            let points = series(c, &book(), &window())?;
            assert_eq!(points.len(), 1);
            let p = &points[0];
            assert_eq!(p.input, 100);
            assert_eq!(p.output, 20);
            assert_eq!(p.cache_read, 30);
            assert_eq!(p.cache_creation, 10);
            assert_eq!(p.tokens, 160);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn activity_splits_cross_hour_buckets() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            seed(
                c,
                vec![
                    sample_bucket(10, 40, 0, 0, 0),
                    sample_bucket(22, 25, 0, 0, 0),
                ],
                vec![],
            );
            let wide = QueryFilter::from_params(
                Some(Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap()),
                Some(Utc.with_ymd_and_hms(2026, 1, 20, 0, 0, 0).unwrap()),
                None,
                None,
                None,
                None,
                false,
            );
            assert!(wide.use_rollups());
            let act = activity(c, &book(), &wide)?;
            assert_eq!(act.cells.len(), 168);
            let t10 = Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap();
            let t22 = Utc.with_ymd_and_hms(2026, 1, 15, 22, 0, 0).unwrap();
            let cell = |t: DateTime<Utc>| -> (u8, u8) {
                (t.weekday().num_days_from_sunday() as u8, t.hour() as u8)
            };
            let (d10, h10) = cell(t10);
            let (d22, h22) = cell(t22);
            assert_ne!((d10, h10), (d22, h22));
            let find = |dow: u8, hour: u8| {
                act.cells
                    .iter()
                    .find(|c| c.dow == dow && c.hour == hour)
                    .unwrap()
            };
            assert_eq!(find(d10, h10).tokens, 40);
            assert_eq!(find(d22, h22).tokens, 25);
            assert_eq!(act.cells.iter().map(|c| c.tokens).sum::<i64>(), 65);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn distributions_group_four_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            seed(c, vec![sample_bucket(10, 100, 0, 0, 0)], vec![]);
            let d = distributions(c, &book(), &window())?;
            assert_eq!(d.host.len(), 1);
            assert_eq!(d.host[0].key, "host1");
            assert_eq!(d.host[0].tokens, 100);
            assert_eq!(d.source[0].key, "codex");
            assert_eq!(d.model[0].key, "gpt-5.4");
            assert_eq!(d.project[0].key, "demo");
            assert!((d.host[0].share - 1.0).abs() < f64::EPSILON);
            assert!(d.model[0].pricing.is_some());
            assert!(d.host[0].pricing.is_none());
            assert!(d.source[0].pricing.is_none());
            Ok(())
        })
        .unwrap();
    }

    fn bucket_model(model: &str, input: i64) -> UsageBucket {
        let mut b = sample_bucket(10, input, 0, 0, 0);
        b.model = model.into();
        b
    }

    #[test]
    fn distributions_fold_effort_keep_fast() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            seed(
                c,
                vec![
                    bucket_model("cursor-grok-4.6-xhigh-fast", 100),
                    bucket_model("cursor-grok-4.6-high-fast", 40),
                    bucket_model("cursor-grok-4.6-high", 20),
                    bucket_model("cursor-grok-4.6-xhigh", 30),
                    bucket_model("grok-4.6-build", 15),
                    bucket_model("gpt-5.6-sol-max", 6),
                    bucket_model("gpt-5.6-sol", 4),
                    bucket_model("claude-fable-5-thinking-max", 8),
                ],
                vec![],
            );
            let d = distributions(c, &book(), &window())?;
            let keys: Vec<&str> = d.model.iter().map(|i| i.key.as_str()).collect();
            assert_eq!(
                keys,
                vec!["grok-4.6-fast", "grok-4.6", "gpt-5.6-sol", "claude-fable-5"]
            );
            assert_eq!(d.model[0].tokens, 140);
            assert_eq!(d.model[1].tokens, 65);
            assert_eq!(d.model[2].tokens, 10);
            assert_eq!(d.model[3].tokens, 8);
            let prices = book();
            let fast = prices.lookup("cursor-grok-4.6-xhigh-fast").unwrap();
            assert_eq!(
                d.model[0].pricing.as_ref().map(|p| p.input),
                Some(fast.input)
            );
            assert_eq!(
                d.model[0].pricing.as_ref().map(|p| p.output),
                Some(fast.output)
            );
            assert!(d.host[0].pricing.is_none());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn model_filter_matches_folded_and_raw_slugs() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            seed(
                c,
                vec![
                    bucket_model("cursor-grok-4.6-xhigh-fast", 100),
                    bucket_model("cursor-grok-4.6-high", 50),
                ],
                vec![],
            );
            let mut folded = window();
            folded.models = vec!["cursor-grok-4.6-fast".into()];
            let d = distributions(c, &book(), &folded)?;
            assert_eq!(d.model.len(), 1);
            assert_eq!(d.model[0].key, "grok-4.6-fast");
            assert_eq!(d.model[0].tokens, 100);

            let mut raw = window();
            raw.models = vec!["cursor-grok-4.6-xhigh-fast".into()];
            let d = distributions(c, &book(), &raw)?;
            assert_eq!(d.model[0].tokens, 100);

            let mut family = window();
            family.models = vec!["grok-4.6".into()];
            let d = distributions(c, &book(), &family)?;
            assert_eq!(d.model[0].key, "grok-4.6");
            assert_eq!(d.model[0].tokens, 50);

            let opts = filter_options(c, &window())?;
            assert_eq!(
                opts.models,
                vec!["grok-4.6".to_string(), "grok-4.6-fast".into()]
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn cost_follows_raw_slug_not_display_name() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.sqlite")).unwrap();
        db.with(|c| {
            seed(
                c,
                vec![bucket_model("cursor-grok-4.6-xhigh-fast", 1_000_000)],
                vec![],
            );
            let s = summary(c, &book(), &window())?;
            let tokens = TokenSlice {
                input: 1_000_000,
                ..TokenSlice::default()
            };
            let fast = book()
                .cost_usd("cursor-grok-4.6-xhigh-fast", tokens)
                .unwrap();
            let std = book().cost_usd("grok-4.6", tokens).unwrap();
            assert!((s.cost_usd - fast).abs() < 1e-9);
            assert!(fast > std);
            Ok(())
        })
        .unwrap();
    }
}
