//! Cursor 信用余额缓存：agent 在 Cursor 同步周期（与全量 CSV 同频）拉一次
//! `get-client-visible-credit-grants`，结果落到 `cache/cursor-credits.json`，
//! 面板只读文件展示，不再每分钟打这个接口。文件不含凭证。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use ai_usage_parsers::CursorAccountSnapshot;

const FILE: &str = "cursor-credits.json";

/// 某账号最近一次拉到的信用余额。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreditEntry {
    pub fetched_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cents: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 服务端 `grants[]` 原样，供弹窗逐条展示。
    #[serde(default)]
    pub grants: Vec<serde_json::Value>,
}

impl CreditEntry {
    /// 从叠加过 `credit_overlay` 的快照与原始 JSON 构造。
    pub fn from_snapshot(snap: &CursorAccountSnapshot, raw: &serde_json::Value) -> Self {
        Self {
            fetched_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            remaining_cents: snap.credit_remaining_cents,
            total_cents: snap.credit_total_cents,
            expires_at: snap.credit_expires_at.clone(),
            label: snap.credit_label.clone(),
            grants: raw
                .get("grants")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// 有可展示的额度（总额 > 0）。没有 grant 的账号不出按钮。
    pub fn has_credit(&self) -> bool {
        self.total_cents.is_some_and(|t| t > 0)
    }
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("cache").join(FILE)
}

pub fn load(data_dir: &Path) -> HashMap<String, CreditEntry> {
    let p = path(data_dir);
    let Ok(raw) = std::fs::read_to_string(&p) else {
        return HashMap::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// 整体覆盖写入：本轮拉到的账号更新，未拉到的保留上次；已删除的账号由调用方传入
/// `keep` 过滤掉。
pub fn store(
    data_dir: &Path,
    fresh: &HashMap<String, CreditEntry>,
    keep: impl Fn(&str) -> bool,
) -> Result<()> {
    let mut all = load(data_dir);
    all.retain(|h, _| keep(h));
    for (h, e) in fresh {
        all.insert(h.clone(), e.clone());
    }
    let p = path(data_dir);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&all)?)
        .with_context(|| format!("写入 {}", tmp.display()))?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(total: i64) -> CreditEntry {
        CreditEntry {
            fetched_at: "2026-09-04T00:00:00Z".into(),
            remaining_cents: Some(total / 2),
            total_cents: Some(total),
            expires_at: None,
            label: None,
            grants: vec![],
        }
    }

    #[test]
    fn store_merges_and_drops_removed_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = HashMap::new();
        a.insert("a".to_string(), entry(100));
        a.insert("gone".to_string(), entry(50));
        store(dir.path(), &a, |_| true).unwrap();
        let mut b = HashMap::new();
        b.insert("b".to_string(), entry(200));
        store(dir.path(), &b, |h| h != "gone").unwrap();
        let all = load(dir.path());
        assert_eq!(all.len(), 2);
        assert_eq!(all["a"].total_cents, Some(100));
        assert_eq!(all["b"].total_cents, Some(200));
        assert!(!all.contains_key("gone"));
    }

    #[test]
    fn from_snapshot_keeps_grants_and_flags_credit() {
        let raw = serde_json::json!({"grants":[
            {"remainingCents":"8415","totalCents":"10000","expiresAtMs":"1788462409347","displayName":"Cursor Grok 4.6 Credit"}
        ]});
        let mut snap = CursorAccountSnapshot::default();
        ai_usage_parsers::credit_overlay(&mut snap, &raw);
        let e = CreditEntry::from_snapshot(&snap, &raw);
        assert!(e.has_credit());
        assert_eq!(e.grants.len(), 1);
        assert_eq!(e.remaining_cents, Some(8415));
        let none =
            CreditEntry::from_snapshot(&CursorAccountSnapshot::default(), &serde_json::json!({}));
        assert!(!none.has_credit());
        assert!(load(std::path::Path::new("/nonexistent")).is_empty());
    }
}
