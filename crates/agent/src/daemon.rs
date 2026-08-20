use std::path::Path;
#[allow(unused_imports)]
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::AgentConfig;

pub fn run_loop(cfg: &AgentConfig, data_dir: &Path, interval: Duration) -> Result<()> {
    loop {
        match crate::sync::run_sync(cfg, data_dir, false) {
            Ok(report) => {
                for line in &report.parser_lines {
                    eprintln!("  {line}");
                }
                if report.changed_buckets + report.changed_sessions > 0 {
                    eprintln!(
                        "已同步 {} buckets · {} sessions",
                        report.ingested, report.sessions
                    );
                }
            }
            Err(err) => eprintln!("同步失败: {err:#}"),
        }
        std::thread::sleep(interval);
    }
}

pub fn install(exe: &Path, config: &Path, data_dir: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        return install_systemd(exe, config, data_dir);
    }
    #[cfg(target_os = "macos")]
    {
        return install_launchd(exe, config, data_dir);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (exe, config, data_dir);
        anyhow::bail!("当前平台不支持 daemon install，请使用 `ai-usage-agent daemon` 前台运行");
    }
}

pub fn uninstall() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        return uninstall_systemd();
    }
    #[cfg(target_os = "macos")]
    {
        return uninstall_launchd();
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        anyhow::bail!("当前平台没有可卸载的 user daemon");
    }
}

pub fn status() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "status", "ai-usage-agent.service"])
            .status();
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("launchctl")
            .args(["print", "gui/$(id -u)/com.ai-usage.agent"])
            .status();
        return Ok(());
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        println!("无 user daemon");
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn install_systemd(exe: &Path, config: &Path, data_dir: &Path) -> Result<()> {
    let path = {
        let dir = crate::xdg::home_dir().join(".config/systemd/user");
        std::fs::create_dir_all(&dir)?;
        dir.join("ai-usage-agent.service")
    };
    let unit = format!(
        "[Unit]\nDescription=AI Usage agent (user)\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={exe} daemon --config {config} --data-dir {data}\nRestart=on-failure\nRestartSec=30\n\n[Install]\nWantedBy=default.target\n",
        exe = shell_escape(exe),
        config = shell_escape(config),
        data = shell_escape(data_dir),
    );
    std::fs::write(&path, unit)?;
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    let st = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "ai-usage-agent.service"])
        .status()
        .context("systemctl --user enable")?;
    if !st.success() {
        anyhow::bail!("systemctl enable 失败（单元已写入 {}）", path.display());
    }
    println!("已安装 user systemd 单元: {}", path.display());
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_systemd() -> Result<()> {
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", "ai-usage-agent.service"])
        .status();
    let path = crate::xdg::home_dir().join(".config/systemd/user/ai-usage-agent.service");
    if path.exists() {
        std::fs::remove_file(&path)?;
        println!("已删除 {}", path.display());
    }
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    Ok(())
}

#[cfg(target_os = "macos")]
fn plist_path() -> PathBuf {
    crate::xdg::home_dir().join("Library/LaunchAgents/com.ai-usage.agent.plist")
}

#[cfg(target_os = "macos")]
fn install_launchd(exe: &Path, config: &Path, data_dir: &Path) -> Result<()> {
    let path = plist_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.ai-usage.agent</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>daemon</string>
    <string>--config</string>
    <string>{}</string>
    <string>--data-dir</string>
    <string>{}</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict>
</plist>
"#,
        exe.display(),
        config.display(),
        data_dir.display()
    );
    std::fs::write(&path, plist)?;
    let _ = std::process::Command::new("launchctl")
        .args(["unload", path.to_str().unwrap()])
        .status();
    let st = std::process::Command::new("launchctl")
        .args(["load", path.to_str().unwrap()])
        .status()?;
    if !st.success() {
        anyhow::bail!("launchctl load 失败");
    }
    println!("已安装 LaunchAgent: {}", path.display());
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_launchd() -> Result<()> {
    let path = plist_path();
    let _ = std::process::Command::new("launchctl")
        .args(["unload", &path.to_string_lossy()])
        .status();
    if path.exists() {
        std::fs::remove_file(&path)?;
        println!("已删除 {}", path.display());
    }
    Ok(())
}

fn shell_escape(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.chars().any(|c| c.is_whitespace()) {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}
