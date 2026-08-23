use std::collections::HashMap;

use ai_usage_protocol::{session_hash_from_id, UsageSession};

use crate::util::{TimingEvent, UsageEntry};

#[derive(Default, Clone, Copy)]
struct TokenSum {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_creation: i64,
    reasoning: i64,
}

impl TokenSum {
    fn add(&mut self, e: &UsageEntry) {
        self.input += e.input_tokens;
        self.output += e.output_tokens;
        self.cache_read += e.cache_read_input_tokens;
        self.cache_creation += e.cache_creation_input_tokens;
        self.reasoning += e.reasoning_output_tokens;
    }
}

pub fn extract_sessions(events: &[TimingEvent], entries: &[UsageEntry]) -> Vec<UsageSession> {
    let mut tokens: HashMap<String, TokenSum> = HashMap::new();
    for e in entries {
        if e.session_id.is_empty() {
            continue;
        }
        tokens.entry(e.session_id.clone()).or_default().add(e);
    }
    let mut groups: HashMap<String, Vec<&TimingEvent>> = HashMap::new();
    for e in events {
        groups.entry(e.session_id.clone()).or_default().push(e);
    }
    let mut sessions = Vec::new();
    for (session_id, mut evs) in groups {
        evs.sort_by_key(|e| e.timestamp);
        let Some(first) = evs.first() else { continue };
        let last = evs.last().unwrap();
        let duration_seconds = (last.timestamp - first.timestamp).num_seconds().max(0);
        let mut active_seconds = 0i64;
        let mut turn_start: Option<chrono::DateTime<chrono::Utc>> = None;
        let mut turn_end: Option<chrono::DateTime<chrono::Utc>> = None;
        let mut waiting = false;
        let mut user_message_count = 0i64;
        for event in &evs {
            if event.role == "user" {
                if let (Some(s), Some(e)) = (turn_start, turn_end) {
                    if e > s {
                        active_seconds += (e - s).num_seconds();
                    }
                }
                turn_start = None;
                turn_end = None;
                waiting = true;
                user_message_count += 1;
            } else if waiting {
                turn_start = Some(event.timestamp);
                turn_end = Some(event.timestamp);
                waiting = false;
            } else if turn_start.is_some() {
                turn_end = Some(event.timestamp);
            }
        }
        if let (Some(s), Some(e)) = (turn_start, turn_end) {
            if e > s {
                active_seconds += (e - s).num_seconds();
            }
        }
        let t = tokens.get(&session_id).cloned().unwrap_or_default();
        sessions.push(
            UsageSession {
                source: first.source.clone(),
                project: if first.project.is_empty() {
                    "unknown".into()
                } else {
                    first.project.clone()
                },
                session_hash: session_hash_from_id(&session_id),
                first_message_at: first.timestamp,
                last_message_at: last.timestamp,
                duration_seconds,
                active_seconds,
                message_count: evs.len() as i64,
                user_message_count,
                input_tokens: t.input,
                output_tokens: t.output,
                cache_read_input_tokens: t.cache_read,
                cache_creation_input_tokens: t.cache_creation,
                reasoning_output_tokens: t.reasoning,
                total_tokens: 0,
            }
            .normalize(),
        );
    }
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn ev(sid: &str, role: &str, minute: u32) -> TimingEvent {
        TimingEvent {
            session_id: sid.into(),
            source: "codex".into(),
            project: "demo".into(),
            timestamp: Utc.with_ymd_and_hms(2026, 1, 15, 10, minute, 0).unwrap(),
            role: role.into(),
        }
    }

    fn entry(sid: &str, input: i64, output: i64) -> UsageEntry {
        UsageEntry {
            source: "codex".into(),
            model: "gpt".into(),
            project: "demo".into(),
            timestamp: Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap(),
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            reasoning_output_tokens: 0,
            session_id: sid.into(),
        }
    }

    #[test]
    fn sums_tokens_per_session_id() {
        let sessions = extract_sessions(
            &[ev("a", "user", 0), ev("a", "assistant", 1), ev("b", "user", 2)],
            &[entry("a", 10, 4), entry("a", 2, 1), entry("b", 7, 0), entry("", 99, 99)],
        );
        let a = sessions
            .iter()
            .find(|s| s.session_hash == session_hash_from_id("a"))
            .unwrap();
        let b = sessions
            .iter()
            .find(|s| s.session_hash == session_hash_from_id("b"))
            .unwrap();
        assert_eq!(a.input_tokens, 12);
        assert_eq!(a.output_tokens, 5);
        assert_eq!(a.total_tokens, 17);
        assert_eq!(b.input_tokens, 7);
        assert_eq!(b.total_tokens, 7);
    }
}
