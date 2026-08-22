use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{account_label, extract_cursor_jwt, CursorTokenPreview};

const TOKEN_KEYS: &[&str] = &[
    "access_token",
    "accessToken",
    "WorkosCursorSessionToken",
    "sessionToken",
];

const SKIP_KEYS: &[&str] = &["refresh_token", "refreshToken"];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CursorAccountSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub membership: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_cycle_end: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_percent: Option<f64>,
    /// Included API pool, cents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_used: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub included_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bonus_cents: Option<i64>,
    /// Auto pool in cents: autoPercentUsed × breakdown.total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_used: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_limit: Option<i64>,
}

impl CursorAccountSnapshot {
    pub fn is_empty(&self) -> bool {
        self.membership.is_none()
            && self.subscription_status.is_none()
            && self.billing_cycle_end.is_none()
            && self.api_percent.is_none()
            && self.auto_percent.is_none()
            && self.plan_used.is_none()
            && self.plan_limit.is_none()
            && self.included_cents.is_none()
            && self.bonus_cents.is_none()
            && self.auto_used.is_none()
            && self.auto_limit.is_none()
    }
}

/// Parse `GET /api/usage-summary` (or an equivalent object) into the panel snapshot.
pub fn snapshot_from_usage_json(v: &Value) -> CursorAccountSnapshot {
    v.as_object().map(usage_snapshot).unwrap_or_default()
}

/// Pull every Cursor access token from a pasted JWT, session cookie, or account dump JSON.
pub fn extract_cursor_previews(raw: &str) -> Vec<CursorTokenPreview> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    if raw.starts_with('{') || raw.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Value>(raw) {
            let mut out = Vec::new();
            let mut seen = HashSet::new();
            walk(&v, &mut out, &mut seen);
            if !out.is_empty() {
                return out;
            }
        }
    }
    if let Some(p) = super::preview_cursor_token(raw) {
        return vec![p];
    }
    Vec::new()
}

fn walk(value: &Value, out: &mut Vec<CursorTokenPreview>, seen: &mut HashSet<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                walk(item, out, seen);
            }
        }
        Value::Object(map) => {
            if let Some(preview) = preview_from_object(map) {
                if seen.insert(preview.account_hash.clone()) {
                    out.push(preview);
                }
            }
            for (key, child) in map {
                if SKIP_KEYS.iter().any(|k| k.eq_ignore_ascii_case(key)) {
                    continue;
                }
                walk(child, out, seen);
            }
        }
        Value::String(s) => {
            if let Some(preview) = super::preview_cursor_token(s) {
                if seen.insert(preview.account_hash.clone()) {
                    out.push(preview);
                }
            }
        }
        _ => {}
    }
}

fn preview_from_object(map: &serde_json::Map<String, Value>) -> Option<CursorTokenPreview> {
    let token = token_from_map(map)?;
    let mut preview = super::preview_cursor_token(token)?;
    let email = email_from_map(map);
    if !email.is_empty() {
        preview.account_label = account_label(&email, "", &preview.account_hash);
    }
    Some(preview)
}

fn token_from_map(map: &serde_json::Map<String, Value>) -> Option<&str> {
    for key in TOKEN_KEYS {
        if let Some(s) = map.get(*key).and_then(Value::as_str) {
            if extract_cursor_jwt(s).is_some() {
                return Some(s);
            }
        }
    }
    map.get("cursor_auth_raw")
        .and_then(Value::as_object)
        .and_then(token_from_map)
}

fn email_from_map(map: &serde_json::Map<String, Value>) -> String {
    for key in ["email", "cachedEmail"] {
        if let Some(s) = map.get(key).and_then(Value::as_str) {
            let t = s.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    if let Some(inner) = map.get("cursor_auth_raw").and_then(Value::as_object) {
        return email_from_map(inner);
    }
    String::new()
}

fn usage_snapshot(usage: &serde_json::Map<String, Value>) -> CursorAccountSnapshot {
    let mut snap = CursorAccountSnapshot {
        membership: first_str(usage, &["membershipType", "membership_type"]),
        billing_cycle_end: first_str(usage, &["billingCycleEnd", "billing_cycle_end"]),
        ..CursorAccountSnapshot::default()
    };
    let pool = usage
        .get("individualUsage")
        .and_then(Value::as_object)
        .and_then(|u| u.get("plan").or_else(|| u.get("overall")))
        .and_then(Value::as_object);
    if let Some(plan) = pool {
        snap.api_percent = plan.get("apiPercentUsed").and_then(as_f64);
        snap.auto_percent = plan
            .get("autoPercentUsed")
            .and_then(as_f64)
            .or_else(|| plan.get("totalPercentUsed").and_then(as_f64));
        snap.plan_used = plan.get("used").and_then(as_i64);
        snap.plan_limit = plan.get("limit").and_then(as_i64);
        if let Some(bd) = plan.get("breakdown").and_then(Value::as_object) {
            snap.included_cents = bd.get("included").and_then(as_i64);
            snap.bonus_cents = bd.get("bonus").and_then(as_i64);
            snap.auto_limit = bd.get("total").and_then(as_i64).or_else(|| {
                match (snap.included_cents, snap.bonus_cents) {
                    (Some(a), Some(b)) => Some(a + b),
                    (Some(a), None) => Some(a),
                    _ => None,
                }
            });
            if let (Some(pct), Some(limit)) = (snap.auto_percent, snap.auto_limit) {
                snap.auto_used = Some(((pct / 100.0) * limit as f64).round() as i64);
            }
        }
    }
    if snap.auto_percent.is_none() {
        if let Some(msg) = first_str(usage, &["autoModelSelectedDisplayMessage"]) {
            snap.auto_percent = parse_percent_from_message(&msg);
        }
    }
    if snap.api_percent.is_none() {
        if let Some(msg) = first_str(usage, &["namedModelSelectedDisplayMessage"]) {
            snap.api_percent = parse_percent_from_message(&msg);
        }
    }
    snap
}

fn parse_percent_from_message(msg: &str) -> Option<f64> {
    let pct_idx = msg.find('%')?;
    let before = &msg[..pct_idx];
    let start = before
        .rfind(|c: char| !c.is_ascii_digit() && c != '.')
        .map(|i| i + 1)
        .unwrap_or(0);
    before[start..].parse().ok()
}

fn first_str(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(s) = map.get(*key).and_then(Value::as_str) {
            let t = s.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

fn as_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_u64().map(|n| n as i64))
        .or_else(|| v.as_f64().map(|n| n as i64))
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_i64().map(|n| n as f64))
        .or_else(|| v.as_u64().map(|n| n as f64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::jwt::fake_jwt;

    #[test]
    fn single_jwt_still_works() {
        let token = fake_jwt("user_01", "a@x.com");
        let found = extract_cursor_previews(&token);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].account_label, "a@x.com");
        assert!(found[0].snapshot.is_empty());
    }

    #[test]
    fn dump_array_overlays_email_not_usage() {
        let token = fake_jwt("user_dump", "");
        let dump = serde_json::json!([{
            "email": "q@example.com",
            "access_token": token,
            "refresh_token": "ignore-me",
            "membership_type": "pro",
            "cursor_usage_raw": {
                "individualUsage": { "plan": { "used": 2000, "limit": 2000, "totalPercentUsed": 13.67 } }
            }
        }]);
        let found = extract_cursor_previews(&dump.to_string());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].access_token, token);
        assert_eq!(found[0].account_label, "q@example.com");
        assert!(found[0].snapshot.is_empty());
    }

    #[test]
    fn usage_summary_json_maps_plan_fields() {
        let v = serde_json::json!({
            "membershipType": "pro",
            "billingCycleEnd": "2026-09-12T13:47:51.000Z",
            "individualUsage": {
                "plan": {
                    "used": 2000,
                    "limit": 2000,
                    "apiPercentUsed": 100,
                    "autoPercentUsed": 0.6,
                    "totalPercentUsed": 13.67,
                    "breakdown": { "included": 2000, "bonus": 2716, "total": 4716 }
                }
            }
        });
        let snap = snapshot_from_usage_json(&v);
        assert_eq!(snap.membership.as_deref(), Some("pro"));
        assert_eq!(snap.api_percent, Some(100.0));
        assert_eq!(snap.auto_percent, Some(0.6));
        assert_eq!(snap.plan_used, Some(2000));
        assert_eq!(snap.plan_limit, Some(2000));
        assert_eq!(snap.included_cents, Some(2000));
        assert_eq!(snap.bonus_cents, Some(2716));
        assert_eq!(snap.auto_limit, Some(4716));
        assert_eq!(snap.auto_used, Some(28));
        assert_eq!(
            snap.billing_cycle_end.as_deref(),
            Some("2026-09-12T13:47:51.000Z")
        );
    }

    #[test]
    fn nested_access_token_and_two_accounts() {
        let a = fake_jwt("user_a", "");
        let b = fake_jwt("user_b", "b@x.com");
        let dump = serde_json::json!([
            {
                "email": "a@x.com",
                "cursor_auth_raw": { "accessToken": a, "cachedEmail": "a@x.com" }
            },
            { "access_token": b }
        ]);
        let found = extract_cursor_previews(&dump.to_string());
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].account_label, "a@x.com");
        assert_eq!(found[1].account_label, "b@x.com");
    }

    #[test]
    fn garbage_is_empty() {
        assert!(extract_cursor_previews("not-a-token").is_empty());
        assert!(extract_cursor_previews("{\"foo\":1}").is_empty());
    }

    #[test]
    fn local_account_dump_if_present() {
        let p = std::path::Path::new("/home/huan/test/qesibubeouxjo_2026-08-21.json");
        if !p.exists() {
            return;
        }
        let raw = std::fs::read_to_string(p).unwrap();
        let found = extract_cursor_previews(&raw);
        assert!(
            !found.is_empty(),
            "dump should yield at least one access token"
        );
        assert!(
            found[0].account_label.contains('@'),
            "label should be email from dump"
        );
        assert!(found[0].snapshot.is_empty());
        assert!(found.iter().all(|a| !a.access_token.is_empty()));
    }
}
