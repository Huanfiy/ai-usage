use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default)]
    pub ui_token: String,
    #[serde(default)]
    pub hide_projects: bool,
}

fn default_bind() -> String {
    "127.0.0.1:3847".into()
}

impl Default for DashConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            ui_token: String::new(),
            hide_projects: false,
        }
    }
}

impl DashConfig {
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("读取 {}", path.display()))?;
        Ok(toml::from_str(&raw).context("解析 dash.toml 失败")?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn bind_addr(&self) -> Result<SocketAddr> {
        self.bind.parse().context("无效 bind 地址")
    }

    pub fn is_loopback_bind(&self) -> bool {
        self.bind_addr()
            .map(|a| a.ip().is_loopback())
            .unwrap_or(true)
    }
}

pub fn resolve_config(cli: Option<&PathBuf>) -> PathBuf {
    paths::dash_config(cli.map(|p| p.as_path()))
}

pub fn resolve_data(cli: Option<&PathBuf>) -> PathBuf {
    paths::dash_data(cli.map(|p| p.as_path()))
}
