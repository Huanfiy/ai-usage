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
    let stored = StoredAccount {
        account_hash: preview.account_hash.clone(),
        account_label: preview.account_label.clone(),
        access_token: preview.access_token.clone(),
        snapshot: CursorAccountSnapshot::default(),
    };
    if let Some(existing) = file
        .accounts
        .iter_mut()
        .find(|a| a.account_hash == stored.account_hash)
    {
        *existing = stored.clone();
    } else {
        file.accounts.push(stored.clone());
    }
    save(data_dir, &file)?;
    Ok(stored)
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

pub fn to_env(file: &CursorAccountsFile) -> Vec<CursorExtraAccount> {
    file.accounts
        .iter()
        .map(|a| CursorExtraAccount {
            access_token: a.access_token.clone(),
            account_label: a.account_label.clone(),
        })
        .collect()
}

pub fn public_views(file: &CursorAccountsFile) -> Vec<AccountView> {
    file.accounts
        .iter()
        .map(|a| {
            let exp = preview_cursor_token(&a.access_token).and_then(|p| p.exp);
            AccountView {
                account_hash: a.account_hash.clone(),
                account_label: a.account_label.clone(),
                exp,
                from_ide: false,
                stored: true,
                snapshot: CursorAccountSnapshot::default(),
                usage_raw: None,
            }
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountView {
    pub account_hash: String,
    pub account_label: String,
    pub exp: Option<i64>,
    pub from_ide: bool,
    pub stored: bool,
    #[serde(flatten)]
    pub snapshot: CursorAccountSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_raw: Option<serde_json::Value>,
}

impl AccountView {
    pub fn from_preview(preview: &CursorTokenPreview, from_ide: bool, stored: bool) -> Self {
        Self {
            account_hash: preview.account_hash.clone(),
            account_label: preview.account_label.clone(),
            exp: preview.exp,
            from_ide,
            stored,
            snapshot: preview.snapshot.clone(),
            usage_raw: None,
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
