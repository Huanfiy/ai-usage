use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params_from_iter, Connection};
use serde::Serialize;

use crate::pricing::{PriceBook, TokenSlice};

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
    model: String,
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
    let (table, time_col) = if f.use_rollups() {
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
    let mut params: Vec<String> = vec![
        if f.use_rollups() {
            f.from.date_naive().to_string()
        } else {
            f.from.to_rfc3339()
        },
        if f.use_rollups() {
            f.to.date_naive().to_string()
        } else {
            f.to.to_rfc3339()
        },
    ];
    if let Some(host) = &f.host_id {
        sql.push_str(" AND host_id = ?");
        params.push(host.clone());
    }
    push_in(&mut sql, &mut params, "source", &f.sources);
    push_in(&mut sql, &mut params, "model", &f.models);
    if !f.hide_projects {
        push_in(&mut sql, &mut params, "project", &f.projects);
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(params.iter()), |r| {
            Ok(TokenRow {
                host_id: r.get(0)?,
                source: r.get(1)?,
                model: r.get(2)?,
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
    Ok(rows)
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
        let slice = TokenSlice {
            input: r.input,
            output: r.output,
            cache_read: r.cache_read,
            cache_write: r.cache_write,
            reasoning: r.reasoning,
        };
        if let Some(c) = book.cost_usd(&r.model, slice) {
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
    let sessions = count_sessions(conn, f)?;
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
        sessions,
        hosts: hosts.len() as i64,
        sources: sources.len() as i64,
    })
}

fn count_sessions(conn: &Connection, f: &QueryFilter) -> Result<i64> {
    let mut sql = String::from(
        "SELECT COUNT(*) FROM usage_sessions WHERE last_message_at >= ? AND last_message_at < ?",
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
    let n: i64 = conn.query_row(&sql, params_from_iter(params.iter()), |r| r.get(0))?;
    Ok(n)
}

#[derive(Serialize)]
pub struct SeriesPoint {
    pub t: String,
    pub tokens: i64,
    pub cost_usd: f64,
}

pub fn series(conn: &Connection, book: &PriceBook, f: &QueryFilter) -> Result<Vec<SeriesPoint>> {
    let rows = scan_rows(conn, f)?;
    let mut map: std::collections::BTreeMap<String, (i64, f64)> = std::collections::BTreeMap::new();
    for r in rows {
        let key = r.bucket_start.clone();
        let slice = TokenSlice {
            input: r.input,
            output: r.output,
            cache_read: r.cache_read,
            cache_write: r.cache_write,
            reasoning: r.reasoning,
        };
        let cost = book.cost_usd(&r.model, slice).unwrap_or(0.0);
        let e = map.entry(key).or_insert((0, 0.0));
        e.0 += r.total;
        e.1 += cost;
    }
    Ok(map
        .into_iter()
        .map(|(t, (tokens, cost_usd))| SeriesPoint { t, tokens, cost_usd })
        .collect())
}

#[derive(Serialize)]
pub struct BreakdownItem {
    pub key: String,
    pub tokens: i64,
    pub cost_usd: f64,
    pub share: f64,
}

pub fn breakdown(
    conn: &Connection,
    book: &PriceBook,
    f: &QueryFilter,
    by: &str,
) -> Result<Vec<BreakdownItem>> {
    let rows = scan_rows(conn, f)?;
    let mut map: std::collections::BTreeMap<String, (i64, f64)> = std::collections::BTreeMap::new();
    let mut total = 0i64;
    for r in rows {
        let key = match by {
            "model" => r.model.clone(),
            "project" => r.project.clone(),
            "host" => r.host_id.clone(),
            _ => r.source.clone(),
        };
        let slice = TokenSlice {
            input: r.input,
            output: r.output,
            cache_read: r.cache_read,
            cache_write: r.cache_write,
            reasoning: r.reasoning,
        };
        let cost = book.cost_usd(&r.model, slice).unwrap_or(0.0);
        let e = map.entry(key).or_insert((0, 0.0));
        e.0 += r.total;
        e.1 += cost;
        total += r.total;
    }
    let mut items: Vec<_> = map
        .into_iter()
        .map(|(key, (tokens, cost_usd))| BreakdownItem {
            key,
            tokens,
            cost_usd,
            share: if total > 0 {
                tokens as f64 / total as f64
            } else {
                0.0
            },
        })
        .collect();
    items.sort_by(|a, b| b.tokens.cmp(&a.tokens));
    Ok(items)
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
}

pub fn sessions(conn: &Connection, f: &QueryFilter, limit: i64) -> Result<Vec<SessionRow>> {
    let mut sql = String::from(
        "SELECT host_id, source, project, session_hash, first_message_at, last_message_at,
                duration_seconds, active_seconds, message_count, user_message_count
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
}

pub fn hosts(conn: &Connection) -> Result<Vec<HostRow>> {
    let mut stmt =
        conn.prepare("SELECT host_id, hostname, last_seen, agent_version FROM hosts ORDER BY last_seen DESC")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(HostRow {
                host_id: r.get(0)?,
                hostname: r.get(1)?,
                last_seen: r.get(2)?,
                agent_version: r.get(3)?,
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
