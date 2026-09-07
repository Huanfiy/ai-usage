//! Cursor 会话监控：daemon 按账号周期核对登录会话，自动撤销未锁定的
//! 新会话（锁定 = 可信设备清单，存于 cursor-accounts.toml）。检查状态与
//! 事件记录落 `cache/cursor-session-guard.json`，面板只读展示，可排查；
//! 文件不含凭证。监控是账号级开关，关闭的账号完全不出网。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use ai_usage_parsers::{fetch_sessions, revoke_session, CursorSession, PlanFetchError};

use crate::cursor_accounts::{self, StoredAccount};
use crate::panel::PanelState;

const FILE: &str = "cursor-session-guard.json";
const MAX_EVENTS: usize = 50;
/// 撤销由服务端异步生效：提交后此时限内不重复撤销同一会话，超时仍在则重试。
const PENDING_TTL_SECS: i64 = 600;
/// 没有到期账号时的最长睡眠；有更早到期取更小值，被 poke 时立即醒。
const MAX_WAIT: Duration = Duration::from_secs(60);

/// 某账号的监控状态（缓存文件的一项）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GuardState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// 已提交撤销待生效：session_id → 提交时刻（RFC3339）。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub pending: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<GuardEvent>,
}

/// 监控事件：kind 为 revoked / revoke_failed / unknown_type / auto_locked。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardEvent {
    pub at: String,
    pub kind: String,
    pub session_id: String,
    pub session_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// 面板展示用摘要（随 /v1/status 下发）。
#[derive(Debug, Clone, Serialize)]
pub struct GuardView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_check_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// 监控开启时的下次检测时刻；关闭为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_check_at: Option<String>,
    pub events: Vec<GuardEvent>,
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("cache").join(FILE)
}

pub fn load(data_dir: &Path) -> HashMap<String, GuardState> {
    let Ok(raw) = std::fs::read_to_string(path(data_dir)) else {
        return HashMap::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn store(data_dir: &Path, all: &HashMap<String, GuardState>) -> Result<()> {
    let p = path(data_dir);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(all)?)
        .with_context(|| format!("写入 {}", tmp.display()))?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn push_event(st: &mut GuardState, ev: GuardEvent) {
    st.events.push(ev);
    if st.events.len() > MAX_EVENTS {
        let drop = st.events.len() - MAX_EVENTS;
        st.events.drain(..drop);
    }
}

/// 一轮检查要做的动作。纯函数，便于无网络单测。
#[derive(Debug, Default)]
pub struct Actions<'a> {
    /// current（本采集端）会话未在锁定集合：自动补锁。
    pub auto_lock: Vec<String>,
    /// 未锁定、类型可撤销、不在待生效窗口内：下线。
    pub revoke: Vec<&'a CursorSession>,
    /// 未锁定但类型未知（不可撤销）：只记录。
    pub unknown: Vec<&'a CursorSession>,
}

pub fn plan_actions<'a>(
    sessions: &'a [CursorSession],
    locked: &HashSet<String>,
    pending: &HashMap<String, String>,
    now: DateTime<Utc>,
) -> Actions<'a> {
    let mut out = Actions::default();
    for s in sessions {
        let is_locked = locked.contains(&s.session_id);
        if s.current {
            if !is_locked {
                out.auto_lock.push(s.session_id.clone());
            }
            continue;
        }
        if is_locked {
            continue;
        }
        if let Some(at) = pending.get(&s.session_id) {
            let fresh = DateTime::parse_from_rfc3339(at)
                .map(|t| now.signed_duration_since(t) < chrono::Duration::seconds(PENDING_TTL_SECS))
                .unwrap_or(false);
            if fresh {
                continue;
            }
        }
        if s.type_code.is_none() {
            out.unknown.push(s);
        } else {
            out.revoke.push(s);
        }
    }
    out
}

/// 到期账号与下次唤醒等待。到期账号视为本轮立刻检查，其下次到期按
/// `now + 周期` 计；无监控账号退回 MAX_WAIT。
pub(crate) fn schedule(
    accounts: &[StoredAccount],
    states: &HashMap<String, GuardState>,
    now: DateTime<Utc>,
) -> (Vec<String>, Duration) {
    let mut due = Vec::new();
    let mut next: Option<DateTime<Utc>> = None;
    for a in accounts {
        if !a.session_guard {
            continue;
        }
        let Ok(period) = crate::config::validate_interval(&a.guard_interval) else {
            continue;
        };
        let period =
            chrono::Duration::from_std(period).unwrap_or_else(|_| chrono::Duration::seconds(60));
        let due_at = states
            .get(&a.account_hash)
            .and_then(|s| s.last_check_at.as_deref())
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc) + period)
            .unwrap_or(now);
        let target = if due_at <= now {
            due.push(a.account_hash.clone());
            now + period
        } else {
            due_at
        };
        next = Some(match next {
            Some(n) if n <= target => n,
            _ => target,
        });
    }
    let wait = next
        .map(|t| (t - now).to_std().unwrap_or(Duration::from_secs(1)))
        .unwrap_or(MAX_WAIT)
        .min(MAX_WAIT)
        .max(Duration::from_secs(1));
    (due, wait)
}

/// 一次检查的结果（也是「立即检测」接口的返回体）。
#[derive(Debug, Serialize)]
pub struct CheckOutcome {
    pub sessions: Vec<CursorSession>,
    pub revoked: Vec<String>,
    pub last_check_at: String,
}

#[derive(Debug)]
pub enum CheckError {
    Upstream(PlanFetchError),
    Io(anyhow::Error),
}

fn err_brief(err: PlanFetchError) -> &'static str {
    match err {
        PlanFetchError::Token | PlanFetchError::Auth => "凭证失效，监控暂停，请重新导入",
        PlanFetchError::Network => "拉取超时或网络失败",
        PlanFetchError::Status => "Cursor 接口返回异常",
        PlanFetchError::Parse => "无法解析会话数据",
    }
}

/// 对单账号执行一轮检查：拉会话 → 计算动作 → 撤销未锁定 → 落状态文件。
/// current 会话自动补进锁定集合；拉取失败只记 `last_error`，不撤销。
pub fn check_account(data_dir: &Path, acct: &StoredAccount) -> Result<CheckOutcome, CheckError> {
    let now = Utc::now();
    let now_s = rfc3339(now);
    let mut all = load(data_dir);
    let st = all.entry(acct.account_hash.clone()).or_default();
    let sessions = match fetch_sessions(&acct.access_token) {
        Ok(list) => list,
        Err(err) => {
            st.last_check_at = Some(now_s);
            st.last_error = Some(err_brief(err).to_string());
            let _ = store(data_dir, &all);
            return Err(CheckError::Upstream(err));
        }
    };
    // 已不在列表的 pending 即撤销已生效，清掉
    let live: HashSet<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
    st.pending.retain(|id, _| live.contains(id.as_str()));
    let locked: HashSet<String> = acct.locked_sessions.iter().cloned().collect();
    let actions = plan_actions(&sessions, &locked, &st.pending, now);
    let mut revoked = Vec::new();
    for s in &actions.revoke {
        match revoke_session(&acct.access_token, &s.session_id, &s.session_type) {
            Ok(()) => {
                st.pending.insert(s.session_id.clone(), now_s.clone());
                push_event(
                    st,
                    GuardEvent {
                        at: now_s.clone(),
                        kind: "revoked".into(),
                        session_id: s.session_id.clone(),
                        session_type: s.session_type.clone(),
                        created_at: Some(s.created_at.clone()),
                        detail: None,
                    },
                );
                revoked.push(s.session_id.clone());
            }
            Err(err) => push_event(
                st,
                GuardEvent {
                    at: now_s.clone(),
                    kind: "revoke_failed".into(),
                    session_id: s.session_id.clone(),
                    session_type: s.session_type.clone(),
                    created_at: Some(s.created_at.clone()),
                    detail: Some(err_brief(err).to_string()),
                },
            ),
        }
    }
    for s in &actions.unknown {
        // 未知类型每次检查都会出现，只记一次避免刷屏
        let already = st
            .events
            .iter()
            .any(|e| e.kind == "unknown_type" && e.session_id == s.session_id);
        if !already {
            push_event(
                st,
                GuardEvent {
                    at: now_s.clone(),
                    kind: "unknown_type".into(),
                    session_id: s.session_id.clone(),
                    session_type: s.session_type.clone(),
                    created_at: Some(s.created_at.clone()),
                    detail: Some("未知会话类型，无法自动下线".into()),
                },
            );
        }
    }
    for id in &actions.auto_lock {
        let session_type = sessions
            .iter()
            .find(|s| &s.session_id == id)
            .map(|s| s.session_type.clone())
            .unwrap_or_default();
        push_event(
            st,
            GuardEvent {
                at: now_s.clone(),
                kind: "auto_locked".into(),
                session_id: id.clone(),
                session_type,
                created_at: None,
                detail: Some("本采集端会话自动锁定".into()),
            },
        );
    }
    st.last_check_at = Some(now_s.clone());
    st.last_error = None;
    store(data_dir, &all).map_err(CheckError::Io)?;
    if !actions.auto_lock.is_empty() {
        cursor_accounts::lock_sessions(data_dir, &acct.account_hash, &actions.auto_lock)
            .map_err(CheckError::Io)?;
    }
    Ok(CheckOutcome {
        sessions,
        revoked,
        last_check_at: now_s,
    })
}

/// 面板摘要：监控关闭且无历史状态时为 None（不出「监控记录」）。
pub fn view(
    st: Option<&GuardState>,
    enabled: bool,
    interval: &str,
    now: DateTime<Utc>,
) -> Option<GuardView> {
    if st.is_none() && !enabled {
        return None;
    }
    let (last_check_at, last_error, events) = st
        .map(|s| (s.last_check_at.clone(), s.last_error.clone(), s.events.clone()))
        .unwrap_or((None, None, Vec::new()));
    let next_check_at = if enabled {
        let period = crate::config::parse_interval(interval)
            .ok()
            .and_then(|d| chrono::Duration::from_std(d).ok());
        match (&last_check_at, period) {
            (Some(t), Some(p)) => DateTime::parse_from_rfc3339(t).ok().map(|t| {
                let n = t.with_timezone(&Utc) + p;
                rfc3339(if n < now { now } else { n })
            }),
            _ => Some(rfc3339(now)),
        }
    } else {
        None
    };
    Some(GuardView {
        last_check_at,
        last_error,
        next_check_at,
        events,
    })
}

/// daemon 守护线程：按各账号 guard_interval 到期检查；设置变更经
/// `poke_guard` 立即唤醒。测试环境不启动。
pub fn spawn(state: Arc<PanelState>) {
    if cfg!(test) {
        return;
    }
    std::thread::spawn(move || loop {
        let wait = run_once(&state);
        state.guard_wait(wait);
    });
}

fn run_once(state: &PanelState) -> Duration {
    let file = cursor_accounts::load(&state.data_dir).unwrap_or_default();
    let states = load(&state.data_dir);
    let (due, wait) = schedule(&file.accounts, &states, Utc::now());
    for hash in due {
        let Some(acct) = file.accounts.iter().find(|a| a.account_hash == hash) else {
            continue;
        };
        let _busy = state.guard_check_lock();
        match check_account(&state.data_dir, acct) {
            Ok(out) if !out.revoked.is_empty() => {
                eprintln!(
                    "会话监控 {}: 已提交下线 {} 条未锁定会话",
                    acct.account_label,
                    out.revoked.len()
                );
            }
            Ok(_) => {}
            Err(CheckError::Upstream(err)) => {
                eprintln!("会话监控 {}: {}", acct.account_label, err_brief(err));
            }
            Err(CheckError::Io(err)) => {
                eprintln!("会话监控 {}: 状态写入失败: {err:#}", acct.account_label);
            }
        }
    }
    wait
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess(id: &str, current: bool, type_code: Option<u32>) -> CursorSession {
        CursorSession {
            session_id: id.into(),
            session_type: if type_code.is_some() {
                "SESSION_TYPE_WEB".into()
            } else {
                "SESSION_TYPE_FUTURE".into()
            },
            type_code,
            created_at: "2026-09-01T00:00:00Z".into(),
            expires_at: None,
            current,
        }
    }

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn plan_actions_locks_current_and_revokes_unlocked() {
        let sessions = vec![
            sess("cur1", true, Some(2)),
            sess("aaa", false, Some(1)),
            sess("bbb", false, Some(1)),
            sess("ccc", false, None),
        ];
        let locked: HashSet<String> = ["bbb".to_string()].into();
        let now = t("2026-09-07T12:00:00Z");
        let out = plan_actions(&sessions, &locked, &HashMap::new(), now);
        assert_eq!(out.auto_lock, vec!["cur1"]);
        assert_eq!(
            out.revoke.iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>(),
            vec!["aaa"]
        );
        assert_eq!(
            out.unknown.iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>(),
            vec!["ccc"]
        );
        // current 已锁定则无需补锁
        let locked: HashSet<String> = ["cur1".to_string(), "aaa".to_string(), "bbb".to_string()].into();
        let out = plan_actions(&sessions, &locked, &HashMap::new(), now);
        assert!(out.auto_lock.is_empty());
        assert!(out.revoke.is_empty());
    }

    #[test]
    fn plan_actions_respects_pending_ttl() {
        let sessions = vec![sess("aaa", false, Some(1))];
        let now = t("2026-09-07T12:00:00Z");
        // 新鲜 pending：跳过
        let mut pending = HashMap::new();
        pending.insert("aaa".to_string(), "2026-09-07T11:55:00Z".to_string());
        let out = plan_actions(&sessions, &HashSet::new(), &pending, now);
        assert!(out.revoke.is_empty());
        // 超时仍在：重试
        pending.insert("aaa".to_string(), "2026-09-07T11:40:00Z".to_string());
        let out = plan_actions(&sessions, &HashSet::new(), &pending, now);
        assert_eq!(out.revoke.len(), 1);
        // 无法解析的时间戳视为过期，照样重试
        pending.insert("aaa".to_string(), "garbage".to_string());
        let out = plan_actions(&sessions, &HashSet::new(), &pending, now);
        assert_eq!(out.revoke.len(), 1);
    }

    fn acct(hash: &str, guard: bool, interval: &str) -> StoredAccount {
        StoredAccount {
            account_hash: hash.into(),
            account_label: format!("{hash}@e.com"),
            access_token: "tok".into(),
            added_at: None,
            report_since: None,
            auto_refresh: true,
            refresh_interval: "300s".into(),
            session_guard: guard,
            guard_interval: interval.into(),
            locked_sessions: Vec::new(),
            snapshot: Default::default(),
        }
    }

    fn state_with_last(at: &str) -> GuardState {
        GuardState {
            last_check_at: Some(at.into()),
            ..GuardState::default()
        }
    }

    #[test]
    fn schedule_due_and_wait() {
        let now = t("2026-09-07T12:00:00Z");
        // 监控全关：无到期，退回 60s 上限
        let (due, wait) = schedule(&[acct("a", false, "60s")], &HashMap::new(), now);
        assert!(due.is_empty());
        assert_eq!(wait, MAX_WAIT);
        // 新开启（无状态）立刻到期，等待 = min(周期, 60s)
        let (due, wait) = schedule(&[acct("a", true, "30s")], &HashMap::new(), now);
        assert_eq!(due, vec!["a"]);
        assert_eq!(wait, Duration::from_secs(30));
        // 刚检查过：等剩余时间
        let mut states = HashMap::new();
        states.insert("a".to_string(), state_with_last("2026-09-07T11:59:40Z"));
        let (due, wait) = schedule(&[acct("a", true, "60s")], &states, now);
        assert!(due.is_empty());
        assert_eq!(wait, Duration::from_secs(40));
        // 多账号取最近到期；长周期账号的等待被 60s 封顶
        states.insert("b".to_string(), state_with_last("2026-09-07T11:00:00Z"));
        let accounts = vec![acct("a", true, "60s"), acct("b", true, "2h")];
        let (due, wait) = schedule(&accounts, &states, now);
        assert!(due.is_empty());
        assert_eq!(wait, Duration::from_secs(40));
        let only_long = vec![acct("b", true, "2h")];
        let (due, wait) = schedule(&only_long, &states, now);
        assert!(due.is_empty());
        assert_eq!(wait, MAX_WAIT);
        // 到期账号本轮检查，等待按 now + 周期参与取最小
        states.insert("c".to_string(), state_with_last("2026-09-07T11:00:00Z"));
        let (due, wait) = schedule(&[acct("c", true, "30s")], &states, now);
        assert_eq!(due, vec!["c"]);
        assert_eq!(wait, Duration::from_secs(30));
    }

    #[test]
    fn store_load_roundtrip_and_event_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = GuardState::default();
        for i in 0..(MAX_EVENTS + 10) {
            push_event(
                &mut st,
                GuardEvent {
                    at: "2026-09-07T12:00:00Z".into(),
                    kind: "revoked".into(),
                    session_id: format!("s{i}"),
                    session_type: "SESSION_TYPE_WEB".into(),
                    created_at: None,
                    detail: None,
                },
            );
        }
        assert_eq!(st.events.len(), MAX_EVENTS);
        assert_eq!(st.events[0].session_id, "s10", "裁掉最老的");
        let mut all = HashMap::new();
        all.insert("a".to_string(), st);
        store(dir.path(), &all).unwrap();
        let loaded = load(dir.path());
        assert_eq!(loaded["a"].events.len(), MAX_EVENTS);
        assert!(load(Path::new("/nonexistent")).is_empty());
    }

    #[test]
    fn view_computes_next_check() {
        let now = t("2026-09-07T12:00:00Z");
        // 关且无状态：None
        assert!(view(None, false, "60s", now).is_none());
        // 开且未检查过：下次 = 现在
        let v = view(None, true, "60s", now).unwrap();
        assert_eq!(v.next_check_at.as_deref(), Some("2026-09-07T12:00:00Z"));
        // 开且刚检查：下次 = 上次 + 周期
        let st = state_with_last("2026-09-07T11:59:00Z");
        let v = view(Some(&st), true, "5m", now).unwrap();
        assert_eq!(v.next_check_at.as_deref(), Some("2026-09-07T12:04:00Z"));
        // 已过期：钳到现在
        let v = view(Some(&st), true, "30s", now).unwrap();
        assert_eq!(v.next_check_at.as_deref(), Some("2026-09-07T12:00:00Z"));
        // 关但有历史：保留记录，无下次
        let v = view(Some(&st), false, "60s", now).unwrap();
        assert!(v.next_check_at.is_none());
        assert_eq!(v.last_check_at.as_deref(), Some("2026-09-07T11:59:00Z"));
    }
}
