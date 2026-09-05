use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use ai_usage_protocol::{
    hash_token, JoinCreated, JoinPollResponse, JoinRequest, JoinStatus, JOIN_TTL_SECS,
};

use crate::config::{self, dest_state_key, AgentConfig, Destination};

const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinState {
    pub join_id: String,
    pub claim_secret: String,
    pub confirm_pin: String,
    pub expires_at: String,
}

impl JoinState {
    pub fn expired(&self) -> bool {
        DateTime::parse_from_rfc3339(&self.expires_at)
            .map(|t| t.with_timezone(&Utc) <= Utc::now())
            .unwrap_or(true)
    }
}

pub fn join_path(data_dir: &Path, url: &str) -> std::path::PathBuf {
    data_dir
        .join("join")
        .join(format!("{}.json", dest_state_key(url)))
}

pub fn load(data_dir: &Path, url: &str) -> Option<JoinState> {
    let raw = std::fs::read_to_string(join_path(data_dir, url)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn save(data_dir: &Path, url: &str, st: &JoinState) -> Result<()> {
    let path = join_path(data_dir, url);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(st)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn clear(data_dir: &Path, url: &str) {
    let _ = std::fs::remove_file(join_path(data_dir, url));
}

fn new_claim_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn request_join(dash_url: &str, hostname: &str) -> Result<JoinState> {
    let secret = new_claim_secret();
    let body = JoinRequest {
        hostname: hostname.to_string(),
        agent_version: Some(env!("CARGO_PKG_VERSION").into()),
        claim_hash: hash_token(&secret),
    };
    let created: JoinCreated = ureq::post(&format!("{}/v1/join", config::normalize_url(dash_url)))
        .timeout(HTTP_TIMEOUT)
        .send_json(body)
        .context("申请接入失败")?
        .into_json()
        .context("解析申请响应失败")?;
    let expires = Utc::now() + chrono::Duration::seconds(created.expires_in as i64);
    Ok(JoinState {
        join_id: created.join_id,
        claim_secret: secret,
        confirm_pin: created.confirm_pin,
        expires_at: expires.to_rfc3339(),
    })
}

pub fn poll_join(dash_url: &str, st: &JoinState) -> Result<JoinPollResponse> {
    ureq::get(&format!(
        "{}/v1/join/{}",
        config::normalize_url(dash_url),
        st.join_id
    ))
    .set("Authorization", &format!("Bearer {}", st.claim_secret))
    .timeout(HTTP_TIMEOUT)
    .call()
    .context("领取轮询失败")?
    .into_json()
    .context("解析领取响应失败")
}

/// Reuse a live pending request, otherwise create a new one.
pub fn ensure_join(data_dir: &Path, url: &str, hostname: &str) -> Result<JoinState> {
    if let Some(st) = load(data_dir, url) {
        if !st.expired() {
            return Ok(st);
        }
        clear(data_dir, url);
    }
    let st = request_join(url, hostname)?;
    save(data_dir, url, &st)?;
    Ok(st)
}

/// Start a fresh request, discarding any previous join state (and caller clears token).
pub fn restart_join(data_dir: &Path, url: &str, hostname: &str) -> Result<JoinState> {
    clear(data_dir, url);
    let st = request_join(url, hostname)?;
    save(data_dir, url, &st)?;
    Ok(st)
}

/// Poll unenrolled destinations. Returns URLs that just received a token.
pub fn apply_claims(
    cfg: &mut AgentConfig,
    config_path: &Path,
    data_dir: &Path,
) -> Result<Vec<String>> {
    let dests = cfg.destinations();
    let mut claimed = Vec::new();
    let mut dirty = false;
    for d in dests {
        if d.enrolled() {
            continue;
        }
        let Some(st) = load(data_dir, &d.url) else {
            continue;
        };
        if st.expired() {
            clear(data_dir, &d.url);
            continue;
        }
        match poll_join(&d.url, &st) {
            Ok(resp) => match resp.status {
                JoinStatus::Approved => {
                    if let Some(token) = resp.token.filter(|t| !t.is_empty()) {
                        cfg.upsert_destination(Destination::new(&d.url, token));
                        clear(data_dir, &d.url);
                        claimed.push(d.url);
                        dirty = true;
                    }
                }
                JoinStatus::Denied | JoinStatus::Expired => {
                    clear(data_dir, &d.url);
                }
                JoinStatus::Pending => {}
            },
            Err(_) => {}
        }
    }
    if dirty {
        cfg.save(config_path)?;
    }
    Ok(claimed)
}

pub fn wait_for_claim(dash_url: &str, st: &JoinState, timeout: Duration) -> Result<Option<String>> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match poll_join(dash_url, st) {
            Ok(resp) => match resp.status {
                JoinStatus::Approved => return Ok(resp.token.filter(|t| !t.is_empty())),
                JoinStatus::Denied => anyhow::bail!("看板已拒绝该申请"),
                JoinStatus::Expired => anyhow::bail!("申请已过期，请重新申请"),
                JoinStatus::Pending => {}
            },
            Err(err) => {
                if std::time::Instant::now() >= deadline {
                    return Err(err);
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

pub fn init_timeout() -> Duration {
    Duration::from_secs(JOIN_TTL_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_state_roundtrip_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let url = "http://127.0.0.1:3847";
        let st = JoinState {
            join_id: "abc".into(),
            claim_secret: "secret".into(),
            confirm_pin: "4821".into(),
            expires_at: (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
        };
        save(dir.path(), url, &st).unwrap();
        let loaded = load(dir.path(), url).unwrap();
        assert_eq!(loaded.confirm_pin, "4821");
        assert!(!loaded.expired());
        clear(dir.path(), url);
        assert!(load(dir.path(), url).is_none());
    }
}
