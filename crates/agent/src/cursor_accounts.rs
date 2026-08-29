use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use ai_usage_parsers::{
    extract_cursor_previews, preview_cursor_token, CursorAccountSnapshot, CursorExtraAccount,
    CursorTokenPreview,
};

const FILE: &str = "cursor-accounts.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CursorAccountsFile {
    #[serde(default)]
    pub accounts: Vec<StoredAccount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAccount {
    pub account_hash: String,
    pub account_label: String,
    pub access_token: String,
    /// 加入采集的时刻（RFC3339）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
    /// 统计起始（RFC3339，固定 cutoff）：只上报此刻及之后的用量。
    /// None = 全部历史。新加入账号默认为加入时刻（云端 CSV 是全量导出）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_since: Option<String>,
    #[serde(flatten, default)]
    pub snapshot: CursorAccountSnapshot,
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

pub fn load(data_dir: &Path) -> Result<CursorAccountsFile> {
    let p = path(data_dir);
    if !p.exists() {
        return Ok(CursorAccountsFile::default());
    }
    let raw = std::fs::read_to_string(&p).with_context(|| format!("读取 {}", p.display()))?;
    toml::from_str(&raw).context("解析 cursor-accounts.toml 失败")
}

pub fn save(data_dir: &Path, file: &CursorAccountsFile) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let p = path(data_dir);
    let raw = toml::to_string_pretty(file)?;
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, raw)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &p)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn upsert(data_dir: &Path, preview: &CursorTokenPreview) -> Result<StoredAccount> {
    let mut file = load(data_dir)?;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    if let Some(existing) = file
        .accounts
        .iter_mut()
        .find(|a| a.account_hash == preview.account_hash)
    {
        // 已加入账号只更新凭证与显示名；added_at / report_since 不动，
        // 换 token 不能挪动统计起始。
        existing.account_label = preview.account_label.clone();
        existing.access_token = preview.access_token.clone();
        let stored = existing.clone();
        save(data_dir, &file)?;
        return Ok(stored);
    }
    let stored = StoredAccount {
        account_hash: preview.account_hash.clone(),
        account_label: preview.account_label.clone(),
        access_token: preview.access_token.clone(),
        added_at: Some(now.clone()),
        report_since: Some(now),
        snapshot: CursorAccountSnapshot::default(),
    };
    file.accounts.push(stored.clone());
    save(data_dir, &file)?;
    Ok(stored)
}

/// 修改统计起始：`Some(rfc3339)` 或 `None`（全部历史）。返回是否找到账号。
pub fn set_report_since(data_dir: &Path, hash: &str, since: Option<String>) -> Result<bool> {
    let mut file = load(data_dir)?;
    let Some(acct) = file.accounts.iter_mut().find(|a| a.account_hash == hash) else {
        return Ok(false);
    };
    acct.report_since = since;
    save(data_dir, &file)?;
    Ok(true)
}

pub fn add_from_raw(data_dir: &Path, raw: &str) -> Result<Vec<StoredAccount>> {
    let found = extract_cursor_previews(raw);
    if found.is_empty() {
        anyhow::bail!("无法从粘贴内容或文件中解析 Cursor 凭证");
    }
    let mut out = Vec::new();
    for preview in found {
        out.push(upsert(data_dir, &preview)?);
    }
    Ok(out)
}

pub fn remove(data_dir: &Path, hash: &str) -> Result<bool> {
    let mut file = load(data_dir)?;
    let before = file.accounts.len();
    file.accounts.retain(|a| a.account_hash != hash);
    if file.accounts.len() == before {
        return Ok(false);
    }
    save(data_dir, &file)?;
    Ok(true)
}

pub fn parse_since(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(t) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    // 纯日期按 UTC 0 点
    chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc))
}

pub fn to_env(file: &CursorAccountsFile) -> Vec<CursorExtraAccount> {
    file.accounts
        .iter()
        .map(|a| CursorExtraAccount {
            access_token: a.access_token.clone(),
            account_label: a.account_label.clone(),
            report_since: a.report_since.as_deref().and_then(parse_since),
        })
        .collect()
}

pub fn public_views(file: &CursorAccountsFile) -> Vec<AccountView> {
    file.accounts
        .iter()
        .map(|a| {
            let preview = preview_cursor_token(&a.access_token);
            AccountView {
                account_hash: a.account_hash.clone(),
                account_label: a.account_label.clone(),
                exp: preview.as_ref().and_then(|p| p.exp),
                token_type: preview.and_then(|p| p.token_type),
                from_ide: false,
                stored: true,
                ide_token_differs: false,
                added_at: a.added_at.clone(),
                report_since: a.report_since.clone(),
                snapshot: CursorAccountSnapshot::default(),
                usage_raw: None,
                usage_error: None,
            }
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountView {
    pub account_hash: String,
    pub account_label: String,
    pub exp: Option<i64>,
    /// 凭证类型：`web`（网站登录，Bot 不可用）或原生 token（None/其它）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    pub from_ide: bool,
    pub stored: bool,
    /// IDE 当前 token 与已加入的存量 token 不同（IDE 已换发）。仅提示，
    /// 不自动覆盖——用户点「更新凭证」才会替换。
    pub ide_token_differs: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_since: Option<String>,
    #[serde(flatten)]
    pub snapshot: CursorAccountSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_raw: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_error: Option<String>,
}

impl AccountView {
    pub fn from_preview(preview: &CursorTokenPreview, from_ide: bool, stored: bool) -> Self {
        Self {
            account_hash: preview.account_hash.clone(),
            account_label: preview.account_label.clone(),
            exp: preview.exp,
            token_type: preview.token_type.clone(),
            from_ide,
            stored,
            ide_token_differs: false,
            added_at: None,
            report_since: None,
            snapshot: preview.snapshot.clone(),
            usage_raw: None,
            usage_error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_jwt() -> String {
        // {"sub":"user_t","email":"t@e.com"}
        "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1c2VyX3QiLCJlbWFpbCI6InRAZS5jb20ifQ.sig".into()
    }

    #[test]
    fn upsert_replaces_same_hash() {
        let dir = tempfile::tempdir().unwrap();
        let jwt = sample_jwt();
        let first = add_from_raw(dir.path(), &jwt).unwrap();
        let second = add_from_raw(dir.path(), &jwt).unwrap();
        assert_eq!(first[0].account_hash, second[0].account_hash);
        let file = load(dir.path()).unwrap();
        assert_eq!(file.accounts.len(), 1);
        assert_eq!(file.accounts[0].account_label, "t@e.com");
    }

    #[test]
    fn upsert_sets_added_at_and_update_preserves_since() {
        let dir = tempfile::tempdir().unwrap();
        let jwt = sample_jwt();
        let first = add_from_raw(dir.path(), &jwt).unwrap();
        let added = first[0].added_at.clone().expect("新账号记录加入时间");
        assert_eq!(
            first[0].report_since.as_deref(),
            Some(added.as_str()),
            "默认从加入时刻起报"
        );
        // 用户改成全部历史
        assert!(set_report_since(dir.path(), &first[0].account_hash, None).unwrap());
        // 重新导入（换 token 场景）不得挪动统计起始与加入时间
        let second = add_from_raw(dir.path(), &jwt).unwrap();
        assert_eq!(second[0].added_at.as_deref(), Some(added.as_str()));
        assert!(second[0].report_since.is_none());
        assert!(!set_report_since(dir.path(), "nope", None).unwrap());
    }

    #[test]
    fn parse_since_accepts_date_and_rfc3339() {
        let d = parse_since("2026-08-29").unwrap();
        assert_eq!(d.to_rfc3339(), "2026-08-29T00:00:00+00:00");
        let t = parse_since("2026-08-29T12:30:00+08:00").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-08-29T04:30:00+00:00");
        assert!(parse_since("").is_none());
        assert!(parse_since("not-a-date").is_none());
    }

    #[test]
    fn remove_account() {
        let dir = tempfile::tempdir().unwrap();
        let stored = add_from_raw(dir.path(), &sample_jwt()).unwrap();
        assert!(remove(dir.path(), &stored[0].account_hash).unwrap());
        assert!(load(dir.path()).unwrap().accounts.is_empty());
    }

    #[test]
    fn json_dump_adds_two_tokens_only() {
        let dir = tempfile::tempdir().unwrap();
        let a = sample_jwt();
        let b = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1c2VyX3UiLCJlbWFpbCI6InVAZS5jb20ifQ.sig";
        let dump = serde_json::json!([
            {
                "email": "t@e.com",
                "access_token": a,
                "membership_type": "pro",
                "cursor_usage_raw": {
                    "individualUsage": { "plan": { "used": 10, "limit": 100, "totalPercentUsed": 12.5 } }
                }
            },
            { "access_token": b }
        ]);
        let added = add_from_raw(dir.path(), &dump.to_string()).unwrap();
        assert_eq!(added.len(), 2);
        let file = load(dir.path()).unwrap();
        assert_eq!(file.accounts.len(), 2);
        assert_eq!(file.accounts[0].access_token, a);
        assert!(file.accounts[0].snapshot.is_empty());
        assert_eq!(file.accounts[0].account_label, "t@e.com");
    }
}
