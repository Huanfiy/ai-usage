use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    #[serde(default = "default_interval_local")]
    pub interval_local: String,
    #[serde(default = "default_interval_cursor")]
    pub interval_cursor: String,
    #[serde(default = "default_bind")]
    pub bind: String,
}

fn default_upload_project() -> bool {
    true
}

pub fn default_interval_local() -> String {
    "5m".into()
}

pub fn default_interval_cursor() -> String {
    "30m".into()
}

pub fn default_bind() -> String {
    "127.0.0.1:3848".into()
}

impl AgentConfig {
    pub fn new(url: String, token: String, hostname: String, upload_project: bool) -> Self {
        Self {
            url,
            token,
            hostname,
            upload_project,
            interval_local: default_interval_local(),
            interval_cursor: default_interval_cursor(),
            bind: default_bind(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置失败: {}", path.display()))?;
        let cfg: Self = toml::from_str(&raw).context("解析 agent.toml 失败")?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, raw)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.token.trim().is_empty() || self.url.trim().is_empty() {
            anyhow::bail!("配置不完整，请先运行 `ai-usage-agent init`");
        }
        parse_interval(&self.interval_local).context("interval_local")?;
        parse_interval(&self.interval_cursor).context("interval_cursor")?;
        require_loopback(&self.bind)?;
        Ok(())
    }

    pub fn local_interval(&self) -> Result<Duration> {
        parse_interval(&self.interval_local)
    }

    pub fn cursor_interval(&self) -> Result<Duration> {
        parse_interval(&self.interval_cursor)
    }

    pub fn bind_addr(&self) -> Result<SocketAddr> {
        require_loopback(&self.bind)
    }
}

pub fn parse_interval(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("间隔为空");
    }
    if let Some(num) = s.strip_suffix('s') {
        return Ok(Duration::from_secs(num.parse()?));
    }
    if let Some(num) = s.strip_suffix('m') {
        return Ok(Duration::from_secs(num.parse::<u64>()? * 60));
    }
    if let Some(num) = s.strip_suffix('h') {
        return Ok(Duration::from_secs(num.parse::<u64>()? * 3600));
    }
    Ok(Duration::from_secs(s.parse()?))
}

pub fn require_loopback(bind: &str) -> Result<SocketAddr> {
    let addr: SocketAddr = bind.parse().context("无效 bind 地址")?;
    if !addr.ip().is_loopback() {
        anyhow::bail!("agent 面板只允许绑定回环地址，当前为 {bind}");
    }
    Ok(addr)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_interval_units() {
        assert_eq!(parse_interval("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_interval("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_interval("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_interval("10").unwrap(), Duration::from_secs(10));
    }

    #[test]
    fn old_toml_gets_interval_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        std::fs::write(
            &path,
            "url = \"http://127.0.0.1:3847\"\ntoken = \"aiu_testtoken\"\n",
        )
        .unwrap();
        let cfg = AgentConfig::load(&path).unwrap();
        assert_eq!(cfg.interval_local, "5m");
        assert_eq!(cfg.interval_cursor, "30m");
        assert_eq!(cfg.bind, "127.0.0.1:3848");
    }

    #[test]
    fn rejects_non_loopback_bind() {
        let cfg = AgentConfig {
            bind: "0.0.0.0:3848".into(),
            ..AgentConfig::new(
                "http://127.0.0.1:3847".into(),
                "aiu_token".into(),
                "host".into(),
                true,
            )
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn loopback_v6_ok() {
        require_loopback("[::1]:3848").unwrap();
    }
}
