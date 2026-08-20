use std::path::{Path, PathBuf};

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn xdg_config(override_path: Option<&Path>) -> PathBuf {
    if let Some(p) = override_path {
        return p.to_path_buf();
    }
    config_dir().join("agent.toml")
}

pub fn xdg_data(override_dir: Option<&Path>) -> PathBuf {
    if let Some(p) = override_dir {
        return p.to_path_buf();
    }
    data_dir().join("agent")
}

pub fn config_dir() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_CONFIG_HOME") {
        if !d.trim().is_empty() {
            return PathBuf::from(d).join("ai-usage");
        }
    }
    home_dir().join(".config").join("ai-usage")
}

pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_DATA_HOME") {
        if !d.trim().is_empty() {
            return PathBuf::from(d).join("ai-usage");
        }
    }
    home_dir().join(".local").join("share").join("ai-usage")
}
