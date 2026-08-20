use std::collections::HashMap;

use ai_usage_protocol::{session_hash_from_id, UsageSession};

use crate::util::TimingEvent;

pub fn extract_sessions(events: &[TimingEvent]) -> Vec<UsageSession> {
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
            }
            .normalize(),
        );
    }
    sessions
}
