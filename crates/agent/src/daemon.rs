use std::collections::HashSet;
use std::net::TcpListener;
use std::path::Path;
#[allow(unused_imports)]
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_usage_protocol::SOURCE_CURSOR;
use anyhow::{Context, Result};

use crate::config::AgentConfig;
use crate::panel::{self, LastSyncView, PanelState};
use crate::sync::{self, local_source_ids};

pub fn run_loop(
    cfg: AgentConfig,
    config_path: &Path,
    data_dir: &Path,
    interval_override: Option<Duration>,
) -> Result<()> {
    let addr = cfg.bind_addr()?;
    let listener = TcpListener::bind(addr).with_context(|| format!("绑定面板 {addr} 失败"))?;
    let bound = listener.local_addr()?;
    let state = PanelState::new(cfg, config_path.to_path_buf(), data_dir.to_path_buf());
    let serve_state = std::sync::Arc::clone(&state);
    std::thread::spawn(move || {
        if let Err(err) = panel::serve(listener, serve_state) {
            eprintln!("面板退出: {err:#}");
        }
    });
    if let Some(d) = interval_override {
        println!(
            "daemon 每 {} 对齐钟面同步一次（CLI 覆盖），Ctrl+C 结束",
            format_dur(d)
        );
    } else {
        let cfg = state.config();
        println!(
            "daemon 本地 {} · Cursor {}（对齐钟面），Ctrl+C 结束",
            cfg.interval_local, cfg.interval_cursor
        );
    }
    println!("面板: http://{bound}");

    let mut last_local: Option<SystemTime> = None;
    let mut last_cursor: Option<SystemTime> = None;
    loop {
        let cfg = state.config();
        let (local_d, cursor_d) = if let Some(d) = interval_override {
            (d, d)
        } else {
            (cfg.local_interval()?, cfg.cursor_interval()?)
        };
        let force = state.take_sync_request();
        let now = SystemTime::now();
        let run_local = force || due_aligned(last_local, local_d, now);
        let run_cursor = force || due_aligned(last_cursor, cursor_d, now);

        let mut want = HashSet::new();
        if run_local {
            for id in local_source_ids() {
                want.insert(id.to_string());
            }
        }
        if run_cursor {
            want.insert(SOURCE_CURSOR.to_string());
        }

        if !want.is_empty() {
            match sync::run_sync_filtered(&cfg, data_dir, false, Some(&want)) {
                Ok(report) => {
                    for line in &report.parser_lines {
                        eprintln!("  {line}");
                    }
                    for w in &report.warnings {
                        eprintln!("  {w}");
                    }
                    if report.changed_buckets + report.changed_sessions > 0 {
                        eprintln!(
                            "已同步 {} buckets · {} sessions",
                            report.ingested, report.sessions
                        );
                    }
                    state.record_sync(run_local, run_cursor, LastSyncView::now_ok());
                }
                Err(err) => {
                    eprintln!("同步失败: {err:#}");
                    state.record_sync(
                        run_local,
                        run_cursor,
                        LastSyncView::from_error(&err.to_string()),
                    );
                }
            }
            let now = SystemTime::now();
            if run_local {
                last_local = Some(now);
            }
            if run_cursor {
                last_cursor = Some(now);
            }
        }

        state.wait_timeout(next_wait(SystemTime::now(), local_d, cursor_d));
    }
}

/// Next Unix-aligned instant strictly after `now` (5m → :00/:05/…, 30m → :00/:30).
fn next_aligned(now: SystemTime, period: Duration) -> SystemTime {
    let period_s = period.as_secs().max(1);
    let now_s = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    UNIX_EPOCH + Duration::from_secs((now_s / period_s + 1) * period_s)
}

fn due_aligned(last: Option<SystemTime>, period: Duration, now: SystemTime) -> bool {
    match last {
        None => true,
        Some(last) => now >= next_aligned(last, period),
    }
}

fn aligned_wait(now: SystemTime, period: Duration) -> Duration {
    next_aligned(now, period)
        .duration_since(now)
        .unwrap_or(Duration::from_secs(1))
}

fn next_wait(now: SystemTime, local_d: Duration, cursor_d: Duration) -> Duration {
    let w = aligned_wait(now, local_d).min(aligned_wait(now, cursor_d));
    if w.is_zero() {
        Duration::from_secs(1)
    } else {
        w
    }
}

fn format_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s % 3600 == 0 {
        format!("{}h", s / 3600)
    } else if s % 60 == 0 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn five_min_aligns_to_clock() {
        let five = Duration::from_secs(300);
        // 01:03:20 → 01:05; exactly 01:05 → 01:10
        assert_eq!(next_aligned(t(3800), five), t(3900));
        assert_eq!(next_aligned(t(3900), five), t(4200));
    }

    #[test]
    fn thirty_min_aligns_to_hour() {
        let thirty = Duration::from_secs(1800);
        // 01:00 → 01:30 → 02:00
        assert_eq!(next_aligned(t(3600), thirty), t(5400));
        assert_eq!(next_aligned(t(5400), thirty), t(7200));
        assert_eq!(next_aligned(t(5399), thirty), t(5400));
    }

    #[test]
    fn due_after_startup_then_next_tick() {
        let five = Duration::from_secs(300);
        assert!(due_aligned(None, five, t(3800)));
        let last = t(3800);
        assert!(!due_aligned(Some(last), five, t(3899)));
        assert!(due_aligned(Some(last), five, t(3900)));
    }

    #[test]
    fn wait_picks_sooner_tick() {
        let five = Duration::from_secs(300);
        let thirty = Duration::from_secs(1800);
        // 01:03:20 → local 1:40, cursor 26:40 → 100s
        assert_eq!(next_wait(t(3800), five, thirty), Duration::from_secs(100));
    }
}
