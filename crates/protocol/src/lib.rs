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
pub const SOURCE_CURSOR: &str = "cursor";

pub const KNOWN_SOURCES: &[&str] = &[SOURCE_CLAUDE_CODE, SOURCE_CODEX, SOURCE_GROK, SOURCE_CURSOR];

pub fn is_known_source(source: &str) -> bool {
    KNOWN_SOURCES.contains(&source)
}

/// Account-scoped sources rewrite ingest identity to `acct:<hash>` so the same
/// login on two machines is one row, not two.
pub fn is_account_scoped(source: &str) -> bool {
    source == SOURCE_CURSOR
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestRequest {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub agent_version: Option<String>,
    /// Agent local UTC offset at ingest time, e.g. `+08:00`. Display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
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
    /// SHA-256 prefix of the cloud account id. Empty means a machine-scoped source.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub account_hash: String,
    /// Display label for the account row (email, else a short hash).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub account_label: String,
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

impl UsageBucket {
    pub fn normalize(mut self) -> Self {
        self.source = clamp(&self.source, 64);
        self.model = clamp_nonempty(&self.model, 100);
        self.project = clamp_nonempty(&self.project, 200);
        self.account_hash = clamp(&self.account_hash, 64);
        self.account_label = clamp(&self.account_label, 200);
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
    ///
    /// Account-scoped buckets include `account_hash` so switching logins on one
    /// machine does not collide incremental state.
    pub fn client_key(&self) -> String {
        if self.account_hash.is_empty() {
            format!(
                "{}|{}|{}|{}",
                self.source,
                self.model,
                self.project,
                self.bucket_start.to_rfc3339()
            )
        } else {
            format!(
                "{}|{}|{}|{}|{}",
                self.source,
                self.account_hash,
                self.model,
                self.project,
                self.bucket_start.to_rfc3339()
            )
        }
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
        self.input_tokens = self.input_tokens.max(0);
        self.output_tokens = self.output_tokens.max(0);
        self.cache_read_input_tokens = self.cache_read_input_tokens.max(0);
        self.cache_creation_input_tokens = self.cache_creation_input_tokens.max(0);
        self.reasoning_output_tokens = self.reasoning_output_tokens.max(0);
        self.total_tokens = self.token_score();
        self
    }

    pub fn token_score(&self) -> i64 {
        self.input_tokens
            + self.output_tokens
            + self.cache_read_input_tokens
            + self.cache_creation_input_tokens
            + self.reasoning_output_tokens
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
            &self.input_tokens.to_string(),
            &self.output_tokens.to_string(),
            &self.cache_read_input_tokens.to_string(),
            &self.cache_creation_input_tokens.to_string(),
            &self.reasoning_output_tokens.to_string(),
            &self.total_tokens.to_string(),
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

/// Account identity stored as `hosts.host_id`. `hash` must already be validated.
pub fn account_host_id(hash: &str) -> String {
    format!("acct:{hash}")
}

/// SHA-256 prefix of the cloud subject, same width as [`host_id_from_token`].
pub fn account_hash_from_sub(sub: &str) -> String {
    hex::encode(&Sha256::digest(sub.as_bytes())[..16])
}

/// Exactly 32 lowercase hex chars. Dash must not concatenate untrusted text into `host_id`.
pub fn is_valid_account_hash(hash: &str) -> bool {
    hash.len() == 32 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
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

/// Accept `+HH:MM` / `-HH:MM`. Anything else is dropped.
pub fn normalize_timezone(raw: &str) -> Option<String> {
    let t = raw.trim();
    let b = t.as_bytes();
    if t.len() != 6 || (b[0] != b'+' && b[0] != b'-') || b[3] != b':' {
        return None;
    }
    if !t[1..3].bytes().all(|c| c.is_ascii_digit()) || !t[4..6].bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let h: i32 = t[1..3].parse().ok()?;
    let m: i32 = t[4..6].parse().ok()?;
    if h > 14 || m > 59 {
        return None;
    }
    Some(t.to_string())
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
            account_hash: String::new(),
            account_label: String::new(),
        }
        .normalize();
        assert_eq!(b.content_hash(), b.clone().content_hash());
        assert_eq!(b.total_tokens, 10);
    }

    #[test]
    fn session_token_fields_default_and_hash() {
        let json = r#"{
            "source":"codex",
            "project":"demo",
            "session_hash":"abc",
            "first_message_at":"2026-01-15T10:00:00Z",
            "last_message_at":"2026-01-15T10:01:00Z"
        }"#;
        let s: UsageSession = serde_json::from_str(json).unwrap();
        assert_eq!(s.total_tokens, 0);
        let hashed = s.clone().normalize();
        assert_eq!(hashed.content_hash(), hashed.clone().content_hash());
        let mut with_tokens = hashed.clone();
        with_tokens.input_tokens = 10;
        with_tokens = with_tokens.normalize();
        assert_eq!(with_tokens.total_tokens, 10);
        assert_ne!(hashed.content_hash(), with_tokens.content_hash());
    }

    #[test]
    fn timezone_offset_is_validated() {
        assert_eq!(normalize_timezone("+08:00").as_deref(), Some("+08:00"));
        assert_eq!(normalize_timezone(" -05:30 ").as_deref(), Some("-05:30"));
        assert_eq!(normalize_timezone("UTC+8"), None);
        assert_eq!(normalize_timezone("+25:00"), None);
        let json = r#"{"schema_version":1}"#;
        let req: IngestRequest = serde_json::from_str(json).unwrap();
        assert!(req.timezone.is_none());
    }

    #[test]
    fn missing_account_fields_default_empty() {
        let json = r#"{
            "source":"codex",
            "model":"gpt-5.4",
            "project":"demo",
            "bucket_start":"2026-01-15T10:00:00Z"
        }"#;
        let b: UsageBucket = serde_json::from_str(json).unwrap();
        assert!(b.account_hash.is_empty());
        assert!(b.account_label.is_empty());
    }

    #[test]
    fn account_hash_helpers() {
        let hash = account_hash_from_sub("user_abc");
        assert!(is_valid_account_hash(&hash));
        assert_eq!(hash.len(), 32);
        assert_eq!(account_host_id(&hash), format!("acct:{hash}"));
        assert!(!is_valid_account_hash(""));
        assert!(!is_valid_account_hash(&hash.to_uppercase()));
        assert!(!is_valid_account_hash("gggggggggggggggggggggggggggggggg"));
        assert!(!is_valid_account_hash("abc"));
    }

    #[test]
    fn client_key_includes_account_hash() {
        let ts = DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut b = UsageBucket {
            source: SOURCE_CURSOR.into(),
            model: "composer-1".into(),
            project: "unknown".into(),
            bucket_start: ts,
            input_tokens: 1,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 0,
            account_hash: String::new(),
            account_label: String::new(),
        };
        let machine = b.client_key();
        b.account_hash = account_hash_from_sub("acct-a");
        let a = b.client_key();
        b.account_hash = account_hash_from_sub("acct-b");
        let c = b.client_key();
        assert_ne!(machine, a);
        assert_ne!(a, c);
        assert!(a.contains(&account_hash_from_sub("acct-a")));
    }

    #[test]
    fn account_label_is_clamped() {
        let ts = DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let b = UsageBucket {
            source: SOURCE_CURSOR.into(),
            model: "m".into(),
            project: "p".into(),
            bucket_start: ts,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 0,
            account_hash: "x".repeat(80),
            account_label: "y".repeat(250),
        }
        .normalize();
        assert_eq!(b.account_hash.chars().count(), 64);
        assert_eq!(b.account_label.chars().count(), 200);
    }
}
