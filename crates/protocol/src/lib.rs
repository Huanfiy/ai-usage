//! Ingest protocol: the only coupling between agent and dash.
//!
//! `cache_read` and `cache_creation` stay in separate fields. Merging them would
//! make cost estimates unrecoverable (Anthropic `cache_creation` is 12.5× `cache_read`).

use chrono::{DateTime, Datelike, Timelike, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SCHEMA_VERSION: u32 = 1;
pub const BUCKET_BATCH: usize = 100;
pub const SESSION_BATCH: usize = 500;

pub const SOURCE_CLAUDE_CODE: &str = "claude-code";
pub const SOURCE_CODEX: &str = "codex";
pub const SOURCE_GROK: &str = "grok";

pub const KNOWN_SOURCES: &[&str] = &[SOURCE_CLAUDE_CODE, SOURCE_CODEX, SOURCE_GROK];

pub fn is_known_source(source: &str) -> bool {
    KNOWN_SOURCES.contains(&source)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestRequest {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub agent_version: Option<String>,
    #[serde(default)]
    pub buckets: Vec<UsageBucket>,
    #[serde(default)]
    pub sessions: Vec<UsageSession>,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageBucket {
    pub source: String,
    pub model: String,
    pub project: String,
    pub bucket_start: DateTime<Utc>,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub cache_read_input_tokens: i64,
    #[serde(default)]
    pub cache_creation_input_tokens: i64,
    #[serde(default)]
    pub reasoning_output_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageSession {
    pub source: String,
    pub project: String,
    pub session_hash: String,
    pub first_message_at: DateTime<Utc>,
    pub last_message_at: DateTime<Utc>,
    #[serde(default)]
    pub duration_seconds: i64,
    #[serde(default)]
    pub active_seconds: i64,
    #[serde(default)]
    pub message_count: i64,
    #[serde(default)]
    pub user_message_count: i64,
}

impl UsageBucket {
    pub fn normalize(mut self) -> Self {
        self.source = clamp(&self.source, 64);
        self.model = clamp_nonempty(&self.model, 100);
        self.project = clamp_nonempty(&self.project, 200);
        self.input_tokens = self.input_tokens.max(0);
        self.output_tokens = self.output_tokens.max(0);
        self.cache_read_input_tokens = self.cache_read_input_tokens.max(0);
        self.cache_creation_input_tokens = self.cache_creation_input_tokens.max(0);
        self.reasoning_output_tokens = self.reasoning_output_tokens.max(0);
        self.total_tokens = self.token_score();
        self.bucket_start = round_to_half_hour(self.bucket_start);
        self
    }

    pub fn token_score(&self) -> i64 {
        self.input_tokens
            + self.output_tokens
            + self.cache_read_input_tokens
            + self.cache_creation_input_tokens
            + self.reasoning_output_tokens
    }

    /// Client-side identity (host is assigned by the server from the token).
    pub fn client_key(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.source,
            self.model,
            self.project,
            self.bucket_start.to_rfc3339()
        )
    }

    pub fn content_hash(&self) -> String {
        hash_parts(&[
            &self.input_tokens.to_string(),
            &self.output_tokens.to_string(),
            &self.cache_read_input_tokens.to_string(),
            &self.cache_creation_input_tokens.to_string(),
            &self.reasoning_output_tokens.to_string(),
            &self.total_tokens.to_string(),
        ])
    }
}

impl UsageSession {
    pub fn normalize(mut self) -> Self {
        self.source = clamp(&self.source, 64);
        self.project = clamp_nonempty(&self.project, 200);
        self.session_hash = clamp(&self.session_hash, 64);
        self.duration_seconds = self.duration_seconds.max(0);
        self.active_seconds = self.active_seconds.max(0);
        self.message_count = self.message_count.max(0);
        self.user_message_count = self.user_message_count.max(0);
        self
    }

    pub fn client_key(&self) -> String {
        format!("{}|{}", self.source, self.session_hash)
    }

    pub fn content_hash(&self) -> String {
        hash_parts(&[
            &self.project,
            &self.first_message_at.to_rfc3339(),
            &self.last_message_at.to_rfc3339(),
            &self.duration_seconds.to_string(),
            &self.active_seconds.to_string(),
            &self.message_count.to_string(),
            &self.user_message_count.to_string(),
        ])
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IngestResponse {
    #[serde(default)]
    pub ingested: u64,
    #[serde(default)]
    pub sessions: u64,
    #[serde(default)]
    pub dropped: DroppedStats,
    #[serde(default)]
    pub protected: ProtectedStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DroppedStats {
    #[serde(default)]
    pub buckets: u64,
    #[serde(default)]
    pub unknown_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProtectedStats {
    #[serde(default)]
    pub buckets: u64,
}

pub fn round_to_half_hour(ts: DateTime<Utc>) -> DateTime<Utc> {
    let minute = if ts.minute() < 30 { 0 } else { 30 };
    ts.with_minute(minute)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(ts)
}

pub fn utc_day(ts: DateTime<Utc>) -> String {
    format!("{:04}-{:02}-{:02}", ts.year(), ts.month(), ts.day())
}

pub fn session_hash_from_id(session_id: &str) -> String {
    hash_parts(&[session_id])
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Stable host identity derived from the ingest token. Payload hostnames are display-only.
pub fn host_id_from_token(token: &str) -> String {
    hex::encode(&Sha256::digest(token.as_bytes())[..16])
}

fn hash_parts(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            hasher.update([0u8]);
        }
        hasher.update(p.as_bytes());
    }
    hex::encode(&hasher.finalize()[..8])
}

fn clamp(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        t.chars().take(max).collect()
    }
}

fn clamp_nonempty(s: &str, max: usize) -> String {
    let t = clamp(s, max);
    if t.is_empty() {
        "unknown".into()
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_hour_rounding() {
        let ts = DateTime::parse_from_rfc3339("2026-01-15T10:17:42Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            round_to_half_hour(ts).to_rfc3339(),
            "2026-01-15T10:00:00+00:00"
        );
        let ts = DateTime::parse_from_rfc3339("2026-01-15T10:30:01Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            round_to_half_hour(ts).to_rfc3339(),
            "2026-01-15T10:30:00+00:00"
        );
    }

    #[test]
    fn content_hash_stable() {
        let b = UsageBucket {
            source: "codex".into(),
            model: "gpt-5.4".into(),
            project: "demo".into(),
            bucket_start: round_to_half_hour(Utc::now()),
            input_tokens: 1,
            output_tokens: 2,
            cache_read_input_tokens: 3,
            cache_creation_input_tokens: 0,
            reasoning_output_tokens: 4,
            total_tokens: 0,
        }
        .normalize();
        assert_eq!(b.content_hash(), b.clone().content_hash());
        assert_eq!(b.total_tokens, 10);
    }
}
