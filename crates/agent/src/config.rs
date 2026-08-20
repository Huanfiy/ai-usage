use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::xdg;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub url: String,
    pub token: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default = "default_upload_project")]
    pub upload_project: bool,
}

fn default_upload_project() -> bool {
    true
}

impl AgentConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置失败: {}", path.display()))?;
        let cfg: Self = toml::from_str(&raw).context("解析 agent.toml 失败")?;
        if cfg.token.trim().is_empty() || cfg.url.trim().is_empty() {
            anyhow::bail!("配置不完整，请先运行 `ai-usage-agent init`");
        }
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, raw)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}

pub fn default_hostname() -> String {
    hostname_from_os().trim_end_matches(".local").to_string()
}

fn hostname_from_os() -> String {
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.trim().is_empty() {
            return h;
        }
    }
    if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
        let h = h.trim();
        if !h.is_empty() {
            return h.to_string();
        }
    }
    "localhost".into()
}

pub fn resolve_config_path(cli: Option<&PathBuf>) -> PathBuf {
    xdg::xdg_config(cli.map(|p| p.as_path()))
}

pub fn resolve_data_dir(cli: Option<&PathBuf>) -> PathBuf {
    xdg::xdg_data(cli.map(|p| p.as_path()))
}
