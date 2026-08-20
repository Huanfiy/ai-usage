use std::path::{Path, PathBuf};

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
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

pub fn dash_config(override_path: Option<&Path>) -> PathBuf {
    override_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| config_dir().join("dash.toml"))
}

pub fn dash_data(override_dir: Option<&Path>) -> PathBuf {
    override_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| data_dir().join("dash"))
}
