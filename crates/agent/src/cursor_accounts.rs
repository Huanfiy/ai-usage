use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use ai_usage_parsers::{preview_cursor_token, CursorExtraAccount, CursorTokenPreview};

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

pub fn add_from_raw(data_dir: &Path, raw: &str) -> Result<StoredAccount> {
    let preview =
        preview_cursor_token(raw).ok_or_else(|| anyhow::anyhow!("无法解析 Cursor access token"))?;
    upsert(data_dir, &preview)
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
            }
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountView {
    pub account_hash: String,
    pub account_label: String,
    pub exp: Option<i64>,
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
        assert_eq!(first.account_hash, second.account_hash);
        let file = load(dir.path()).unwrap();
        assert_eq!(file.accounts.len(), 1);
        assert_eq!(file.accounts[0].account_label, "t@e.com");
    }

    #[test]
    fn remove_account() {
        let dir = tempfile::tempdir().unwrap();
        let stored = add_from_raw(dir.path(), &sample_jwt()).unwrap();
        assert!(remove(dir.path(), &stored.account_hash).unwrap());
        assert!(load(dir.path()).unwrap().accounts.is_empty());
    }
}
