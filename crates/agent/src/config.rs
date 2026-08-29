use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::xdg;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Destination {
    pub url: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// 旧单地址字段：只读兼容，载入时并入 `destinations`，保存不再写出。
    #[serde(default, skip_serializing)]
    pub url: String,
    /// 旧单 token 字段：只读兼容，同上。
    #[serde(default, skip_serializing)]
    pub token: String,
    #[serde(default)]
    pub destinations: Vec<Destination>,
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

pub fn normalize_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

impl Destination {
    pub fn new(url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            url: normalize_url(&url.into()),
            token: token.into(),
        }
    }
}

impl AgentConfig {
    pub fn new(url: String, token: String, hostname: String, upload_project: bool) -> Self {
        let dest = Destination::new(url, token);
        Self {
            url: String::new(),
            token: String::new(),
            destinations: vec![dest],
            hostname,
            upload_project,
            interval_local: default_interval_local(),
            interval_cursor: default_interval_cursor(),
            bind: default_bind(),
        }
    }

    /// 空配置：setup 模式下 daemon 也能启动，等待面板补上首个看板地址。
    pub fn empty() -> Self {
        Self {
            url: String::new(),
            token: String::new(),
            destinations: Vec::new(),
            hostname: default_hostname(),
            upload_project: default_upload_project(),
            interval_local: default_interval_local(),
            interval_cursor: default_interval_cursor(),
            bind: default_bind(),
        }
    }

    pub fn destinations(&self) -> Vec<Destination> {
        if !self.destinations.is_empty() {
            return self
                .destinations
                .iter()
                .map(|d| Destination::new(&d.url, &d.token))
                .collect();
        }
        if !self.url.trim().is_empty() {
            return vec![Destination::new(&self.url, &self.token)];
        }
        Vec::new()
    }

    pub fn set_destinations(&mut self, dests: Vec<Destination>) {
        self.destinations = dests
            .into_iter()
            .map(|d| Destination::new(d.url, d.token))
            .collect();
        // 旧字段只在载入时并入一次，此后不再镜像、保存不写出。
        self.url.clear();
        self.token.clear();
    }

    /// 按 URL upsert 一条看板地址；返回是否新增（false = 更新已有 token）。
    pub fn upsert_destination(&mut self, dest: Destination) -> bool {
        let mut dests = self.destinations();
        if let Some(existing) = dests.iter_mut().find(|d| d.url == dest.url) {
            existing.token = dest.token;
            self.set_destinations(dests);
            false
        } else {
            dests.push(dest);
            self.set_destinations(dests);
            true
        }
    }

    pub fn find_dest(&self, url: &str) -> Option<Destination> {
        let key = normalize_url(url);
        self.destinations().into_iter().find(|d| d.url == key)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置失败: {}", path.display()))?;
        let mut cfg: Self = toml::from_str(&raw).context("解析 agent.toml 失败")?;
        cfg.set_destinations(cfg.destinations());
        if cfg.hostname.trim().is_empty() {
            cfg.hostname = default_hostname();
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// setup 模式载入：文件缺失时返回空配置而不是报错；文件存在但损坏仍报错。
    pub fn load_or_setup(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::empty());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置失败: {}", path.display()))?;
        let mut cfg: Self = toml::from_str(&raw).context("解析 agent.toml 失败")?;
        cfg.set_destinations(cfg.destinations());
        if cfg.hostname.trim().is_empty() {
            cfg.hostname = default_hostname();
        }
        cfg.validate_base()?;
        Ok(cfg)
    }

    /// 含 ingest token，权限收紧到 0600（对齐 cursor-accounts.toml）。
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate_base()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, raw)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// 除「至少一个看板地址」外的全部校验；setup 模式的空配置也要能过。
    pub fn validate_base(&self) -> Result<()> {
        let dests = self.destinations();
        let mut seen = std::collections::HashSet::new();
        for d in &dests {
            if d.url.is_empty() {
                anyhow::bail!("看板地址不能为空");
            }
            if !d.url.starts_with("http://") && !d.url.starts_with("https://") {
                anyhow::bail!("看板地址需以 http:// 或 https:// 开头: {}", d.url);
            }
            if d.token.trim().is_empty() {
                anyhow::bail!("看板地址 {} 缺少 ingest token", d.url);
            }
            if !seen.insert(d.url.clone()) {
                anyhow::bail!("看板地址重复: {}", d.url);
            }
        }
        validate_interval(&self.interval_local).context("interval_local")?;
        validate_interval(&self.interval_cursor).context("interval_cursor")?;
        require_loopback(&self.bind)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.destinations().is_empty() {
            anyhow::bail!("尚未配置看板地址：在面板添加，或运行 `ai-usage-agent init`");
        }
        self.validate_base()
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

/// 目的地增量 state 文件名：`state-<sha256(url)[:16]>.json`。
pub fn dest_state_key(url: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(normalize_url(url).as_bytes());
    hex::encode(&digest[..16])
}

pub fn dest_state_path(data_dir: &Path, url: &str) -> PathBuf {
    data_dir.join(format!("state-{}.json", dest_state_key(url)))
}

pub fn parse_interval(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("间隔为空，格式如 30s / 5m / 2h");
    }
    if let Some(num) = s.strip_suffix('s') {
        return Ok(Duration::from_secs(num.parse().context("间隔格式如 30s / 5m / 2h")?));
    }
    if let Some(num) = s.strip_suffix('m') {
        return Ok(Duration::from_secs(
            num.parse::<u64>().context("间隔格式如 30s / 5m / 2h")? * 60,
        ));
    }
    if let Some(num) = s.strip_suffix('h') {
        return Ok(Duration::from_secs(
            num.parse::<u64>().context("间隔格式如 30s / 5m / 2h")? * 3600,
        ));
    }
    Ok(Duration::from_secs(
        s.parse().context("间隔格式如 30s / 5m / 2h")?,
    ))
}

pub const MIN_INTERVAL: Duration = Duration::from_secs(15);
pub const MAX_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// 解析并校验上下限（15s–24h），配置校验与 CLI 覆盖共用。
pub fn validate_interval(s: &str) -> Result<Duration> {
    let d = parse_interval(s)?;
    if d < MIN_INTERVAL || d > MAX_INTERVAL {
        anyhow::bail!("间隔 {s} 超出范围，需在 15s 与 24h 之间");
    }
    Ok(d)
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
        assert_eq!(cfg.destinations().len(), 1);
        assert_eq!(cfg.destinations()[0].url, "http://127.0.0.1:3847");
        assert_eq!(cfg.destinations()[0].token, "aiu_testtoken");
    }

    #[test]
    fn destinations_toml_roundtrip_without_legacy_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        let mut cfg = AgentConfig::new(
            "http://127.0.0.1:3847".into(),
            "aiu_a".into(),
            "host".into(),
            true,
        );
        cfg.set_destinations(vec![
            Destination::new("http://127.0.0.1:3847/", "aiu_a"),
            Destination::new("http://10.0.0.2:3847", "aiu_b"),
        ]);
        cfg.save(&path).unwrap();
        // 保存只写 destinations，不再顶层双写 url / token（顶层 = 首个表头之前）
        let raw = std::fs::read_to_string(&path).unwrap();
        let top = raw.split('[').next().unwrap_or("");
        assert!(!top.contains("url ="), "{raw}");
        assert!(!top.contains("token ="), "{raw}");
        let loaded = AgentConfig::load(&path).unwrap();
        let dests = loaded.destinations();
        assert_eq!(dests.len(), 2);
        assert_eq!(dests[0].url, "http://127.0.0.1:3847");
        assert_eq!(dests[1].token, "aiu_b");
        assert!(loaded.url.is_empty());
        assert!(loaded.token.is_empty());
    }

    #[test]
    fn legacy_single_url_file_migrates_on_resave() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        std::fs::write(
            &path,
            "url = \"http://127.0.0.1:3847\"\ntoken = \"aiu_legacy\"\n",
        )
        .unwrap();
        let cfg = AgentConfig::load(&path).unwrap();
        assert_eq!(cfg.destinations()[0].token, "aiu_legacy");
        cfg.save(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let top = raw.split('[').next().unwrap_or("");
        assert!(!top.contains("url ="), "{raw}");
        assert!(!top.contains("token ="), "{raw}");
        assert!(raw.contains("[[destinations]]"), "{raw}");
        assert!(AgentConfig::load(&path).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn save_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        AgentConfig::new(
            "http://127.0.0.1:3847".into(),
            "aiu_secret".into(),
            "host".into(),
            true,
        )
        .save(&path)
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "agent.toml 含 token 必须 0600");
    }

    #[test]
    fn upsert_destination_updates_or_appends() {
        let mut cfg = AgentConfig::new(
            "http://127.0.0.1:3847".into(),
            "aiu_a".into(),
            "host".into(),
            true,
        );
        let added = cfg.upsert_destination(Destination::new("http://127.0.0.1:3847/", "aiu_new"));
        assert!(!added, "同 URL 是更新不是新增");
        assert_eq!(cfg.destinations()[0].token, "aiu_new");
        let added = cfg.upsert_destination(Destination::new("http://10.0.0.2:3847", "aiu_b"));
        assert!(added);
        assert_eq!(cfg.destinations().len(), 2);
    }

    #[test]
    fn validate_rejects_bad_scheme_and_out_of_range_interval() {
        let mut cfg = AgentConfig::new(
            "ftp://127.0.0.1:3847".into(),
            "aiu_a".into(),
            "host".into(),
            true,
        );
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("http"), "{err}");
        cfg = AgentConfig::new(
            "http://127.0.0.1:3847".into(),
            "aiu_a".into(),
            "host".into(),
            true,
        );
        cfg.interval_local = "5s".into();
        assert!(cfg.validate().is_err(), "低于 15s 下限");
        cfg.interval_local = "25h".into();
        assert!(cfg.validate().is_err(), "超出 24h 上限");
        cfg.interval_local = "15s".into();
        cfg.validate().unwrap();
    }

    #[test]
    fn empty_config_passes_base_but_not_full_validate() {
        let cfg = AgentConfig::empty();
        cfg.validate_base().unwrap();
        assert!(cfg.validate().is_err());
        assert!(!cfg.hostname.is_empty(), "空配置也带默认主机名");
    }

    #[test]
    fn load_or_setup_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = AgentConfig::load_or_setup(&dir.path().join("agent.toml")).unwrap();
        assert!(cfg.destinations().is_empty());
        assert_eq!(cfg.bind, "127.0.0.1:3848");
    }

    #[test]
    fn rejects_duplicate_urls() {
        let mut cfg = AgentConfig::new(
            "http://127.0.0.1:3847".into(),
            "aiu_a".into(),
            "host".into(),
            true,
        );
        cfg.destinations = vec![
            Destination::new("http://127.0.0.1:3847/", "aiu_a"),
            Destination::new("http://127.0.0.1:3847", "aiu_b"),
        ];
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn dest_state_key_stable_after_slash() {
        assert_eq!(
            dest_state_key("http://127.0.0.1:3847/"),
            dest_state_key("http://127.0.0.1:3847")
        );
        assert_ne!(
            dest_state_key("http://127.0.0.1:3847"),
            dest_state_key("http://127.0.0.1:3848")
        );
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
