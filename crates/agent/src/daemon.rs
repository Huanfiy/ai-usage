use std::collections::HashSet;
use std::net::TcpListener;
use std::path::Path;
#[allow(unused_imports)]
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_usage_parsers::CURSOR_NOT_ENROLLED;
use ai_usage_protocol::SOURCE_CURSOR;
use anyhow::{Context, Result};

use crate::config::{AgentConfig, Destination};
use crate::panel::{self, LastSyncView, PanelState, SyncJob};
use crate::sync::{self, local_source_ids, SyncReport};

/// 单档调度状态：`last_ok` 驱动钟面对齐，`retry_at` 驱动失败退避。
/// 可重试失败不推进钟面，退避封顶下一刻度；成功或不值得短退避的
/// 失败（4xx/鉴权/解析类）推进钟面等下一刻度。
#[derive(Default)]
struct Tier {
    last_ok: Option<SystemTime>,
    retry_at: Option<SystemTime>,
    retry_level: u32,
}

impl Tier {
    fn due(&self, period: Duration, now: SystemTime) -> bool {
        if let Some(at) = self.retry_at {
            return now >= at;
        }
        match self.last_ok {
            None => true,
            Some(last) => now >= next_aligned(last, period),
        }
    }

    fn advance(&mut self, now: SystemTime) {
        self.last_ok = Some(now);
        self.retry_at = None;
        self.retry_level = 0;
    }

    fn backoff(&mut self, period: Duration, now: SystemTime) {
        let level = self.retry_level.min(5);
        let base = Duration::from_secs(30u64.saturating_mul(1u64 << level));
        let jitter = Duration::from_millis(jitter_ms());
        let cap = next_aligned(now, period)
            .duration_since(now)
            .unwrap_or(Duration::from_secs(1))
            .max(Duration::from_secs(1));
        let delay = (base + jitter).min(cap);
        self.retry_at = Some(now + delay);
        self.retry_level = self.retry_level.saturating_add(1);
    }

    fn next_due_in(&self, period: Duration, now: SystemTime) -> Duration {
        let target = match self.retry_at {
            Some(at) => at,
            None => match self.last_ok {
                None => return Duration::from_secs(1),
                Some(last) => next_aligned(last, period),
            },
        };
        target
            .duration_since(now)
            .unwrap_or(Duration::from_secs(1))
    }
}

/// 0–10s 抖动，避免多机在同一钟面刻度齐打看板。
fn jitter_ms() -> u64 {
    (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
        % 10_000) as u64
}

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

    let mut local = Tier::default();
    let mut cursor = Tier::default();
    loop {
        let cfg = state.config();
        let (local_d, cursor_d) = if let Some(d) = interval_override {
            (d, d)
        } else {
            (cfg.local_interval()?, cfg.cursor_interval()?)
        };
        let jobs = state.take_sync_jobs();
        let now = SystemTime::now();
        let due_local = local.due(local_d, now);
        let due_cursor = cursor.due(cursor_d, now);
        let scheduled = due_local || due_cursor;

        if jobs.is_empty() && !scheduled {
            state.wait_timeout(wait_hint(now, &local, local_d, &cursor, cursor_d));
            continue;
        }

        // job（手动/新地址）要求全部源；纯调度轮只解析到期档。
        let run_local = due_local || !jobs.is_empty();
        let run_cursor = due_cursor || !jobs.is_empty();
        let mut want = HashSet::new();
        if run_local {
            for id in local_source_ids() {
                want.insert(id.to_string());
            }
        }
        if run_cursor {
            want.insert(SOURCE_CURSOR.to_string());
        }

        let dests = cfg.destinations();
        if dests.is_empty() {
            // setup 模式：还没有看板地址，空转等面板配置，不算错误。
            let view = LastSyncView::with_note("尚未配置看板地址");
            state.record_sync(
                run_local.then(|| view.clone()),
                run_cursor.then(|| view.clone()),
            );
            let now = SystemTime::now();
            if run_local {
                local.advance(now);
            }
            if run_cursor {
                cursor.advance(now);
            }
            state.wait_timeout(wait_hint(now, &local, local_d, &cursor, cursor_d));
            continue;
        }
        let blocked = state.auth_blocked();
        let selected = select_jobs(&dests, &jobs, scheduled, &blocked);

        if selected.is_empty() {
            let view =
                LastSyncView::from_error("所有看板地址均鉴权失败，已暂停上报；请在面板更新 token");
            state.record_sync(
                run_local.then(|| view.clone()),
                run_cursor.then(|| view.clone()),
            );
            let now = SystemTime::now();
            if run_local {
                local.advance(now);
            }
            if run_cursor {
                cursor.advance(now);
            }
            state.wait_timeout(wait_hint(now, &local, local_d, &cursor, cursor_d));
            continue;
        }

        state.set_syncing(true);
        let result = sync::run_sync_jobs(&cfg, data_dir, false, Some(&want), &selected);
        state.set_syncing(false);
        let done = SystemTime::now();

        match result {
            Ok(report) => {
                for line in report.parser_lines() {
                    eprintln!("  {line}");
                }
                for w in report.warnings() {
                    eprintln!("  {w}");
                }
                for e in report.dest_errors() {
                    eprintln!("同步失败: {e}");
                }
                if report.changed() > 0 && report.all_ok() {
                    eprintln!(
                        "已同步 {} buckets · {} sessions",
                        report.ingested(),
                        report.sessions_total()
                    );
                }
                state.record_report(&report);
                let (agent_view, cursor_view) = tier_views(&report, run_local, run_cursor);
                state.record_sync(agent_view, cursor_view);
                if report.retryable_failure() {
                    if run_local {
                        local.backoff(local_d, done);
                    }
                    if run_cursor {
                        cursor.backoff(cursor_d, done);
                    }
                } else {
                    if run_local {
                        local.advance(done);
                    }
                    if run_cursor {
                        cursor.advance(done);
                    }
                }
            }
            Err(err) => {
                eprintln!("同步失败: {err:#}");
                let view = LastSyncView::from_error(&truncate(&format!("{err:#}"), 300));
                state.record_sync(
                    run_local.then(|| view.clone()),
                    run_cursor.then(|| view.clone()),
                );
                if run_local {
                    local.advance(done);
                }
                if run_cursor {
                    cursor.advance(done);
                }
            }
        }
        state.wait_timeout(wait_hint(
            SystemTime::now(),
            &local,
            local_d,
            &cursor,
            cursor_d,
        ));
    }
}

/// job 指定的目标始终包含（显式动作可穿透 401 封锁）；
/// 到期调度补上其余未封锁目标，多个 job 合并 full 标志，只解析一次。
fn select_jobs(
    dests: &[Destination],
    jobs: &[SyncJob],
    scheduled: bool,
    blocked: &HashSet<String>,
) -> Vec<sync::DestJob> {
    let mut out = Vec::new();
    for d in dests {
        let mut explicit = false;
        let mut full = false;
        for j in jobs {
            let matched = match &j.url {
                None => true,
                Some(u) => u == &d.url,
            };
            if matched {
                explicit = true;
                full |= j.full;
            }
        }
        if explicit || (scheduled && !blocked.contains(&d.url)) {
            out.push(sync::DestJob {
                dest: d.clone(),
                full,
            });
        }
    }
    out
}

/// 按档合成「上次同步」视图：目标失败两档共担；解析被 skipped 的源
/// 记入对应档（Cursor 源 → cursor 档，本地源 → agent 档），不再标绿。
fn tier_views(
    report: &SyncReport,
    run_local: bool,
    run_cursor: bool,
) -> (Option<LastSyncView>, Option<LastSyncView>) {
    let dest_errs = report.dest_errors();
    let agent = run_local.then(|| {
        let mut errs = dest_errs.clone();
        for s in &report.sources {
            if s.source != SOURCE_CURSOR && s.skipped && !s.warnings.is_empty() {
                errs.push(s.warnings.join("；"));
            }
        }
        view_from_errs(&errs)
    });
    let cursor = run_cursor.then(|| {
        let mut errs = dest_errs.clone();
        let mut not_enrolled = false;
        if let Some(s) = report.source(SOURCE_CURSOR) {
            if s.skipped {
                if s.warnings.iter().any(|w| w == CURSOR_NOT_ENROLLED) {
                    // 未加入采集账号是空闲状态，不是错误。
                    not_enrolled = true;
                } else if s.warnings.is_empty() {
                    errs.push("Cursor 源本轮未采集".into());
                } else {
                    errs.push(s.warnings.join("；"));
                }
            }
        }
        if errs.is_empty() && not_enrolled {
            LastSyncView::with_note("未加入采集账号")
        } else {
            view_from_errs(&errs)
        }
    });
    (agent, cursor)
}

fn view_from_errs(errs: &[String]) -> LastSyncView {
    if errs.is_empty() {
        LastSyncView::now_ok()
    } else {
        LastSyncView::from_error(&truncate(&errs.join("; "), 500))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

fn wait_hint(
    now: SystemTime,
    local: &Tier,
    local_d: Duration,
    cursor: &Tier,
    cursor_d: Duration,
) -> Duration {
    let w = local
        .next_due_in(local_d, now)
        .min(cursor.next_due_in(cursor_d, now));
    if w.is_zero() {
        Duration::from_secs(1)
    } else {
        w
    }
}

/// Next Unix-aligned instant strictly after `now` (5m → :00/:05/…, 30m → :00/:30).
pub(crate) fn next_aligned(now: SystemTime, period: Duration) -> SystemTime {
    let period_s = period.as_secs().max(1);
    let now_s = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    UNIX_EPOCH + Duration::from_secs((now_s / period_s + 1) * period_s)
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
        let exe = install_binary(exe)?;
        return install_systemd(&exe, config, data_dir);
    }
    #[cfg(target_os = "macos")]
    {
        let exe = install_binary(exe)?;
        return install_launchd(&exe, config, data_dir);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (exe, config, data_dir);
        anyhow::bail!("当前平台不支持 daemon install，请使用 `ai-usage-agent daemon` 前台运行");
    }
}

/// 统一装到 `~/.local/bin/ai-usage-agent`，与 deploy 模板和 `run.sh agent
/// reload` 一致，避免 service 绑在 target/debug 之类的临时路径上。
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn install_binary(exe: &Path) -> Result<PathBuf> {
    let target = crate::xdg::home_dir().join(".local/bin/ai-usage-agent");
    let canon_exe = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
    let canon_target = target.canonicalize().unwrap_or_else(|_| target.clone());
    if canon_exe == canon_target {
        return Ok(target);
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = target.with_extension("new");
    std::fs::copy(&canon_exe, &tmp).with_context(|| format!("复制到 {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &target)?;
    println!("已安装二进制: {}", target.display());
    Ok(target)
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
        // launchctl 不经过 shell，$(id -u) 不会展开，须自行取 uid
        let uid = std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let _ = std::process::Command::new("launchctl")
            .args(["print", &format!("gui/{uid}/com.ai-usage.agent")])
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
    println!("无图形会话的机器需 `loginctl enable-linger $USER`，否则开机不启动、登出即停止。");
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
    use crate::sync::{DestReport, PushErrorKind, SourceReport};

    fn t(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn tier(last_ok: Option<SystemTime>) -> Tier {
        Tier {
            last_ok,
            retry_at: None,
            retry_level: 0,
        }
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
        assert!(tier(None).due(five, t(3800)));
        let tr = tier(Some(t(3800)));
        assert!(!tr.due(five, t(3899)));
        assert!(tr.due(five, t(3900)));
    }

    #[test]
    fn wait_picks_sooner_tick() {
        let five = Duration::from_secs(300);
        let thirty = Duration::from_secs(1800);
        // 01:03:20 → local 1:40, cursor 26:40 → 100s
        let local = tier(Some(t(3800)));
        let cursor = tier(Some(t(3800)));
        assert_eq!(
            wait_hint(t(3800), &local, five, &cursor, thirty),
            Duration::from_secs(100)
        );
    }

    #[test]
    fn backoff_does_not_advance_clock_and_caps_at_tick() {
        let five = Duration::from_secs(300);
        let mut tr = tier(Some(t(3800)));
        // 失败退避：钟面 last_ok 不动
        tr.backoff(five, t(3810));
        assert_eq!(tr.last_ok, Some(t(3800)));
        let at = tr.retry_at.expect("retry scheduled");
        let delay = at.duration_since(t(3810)).unwrap();
        // 首档 30s + 0..10s 抖动
        assert!(delay >= Duration::from_secs(30), "{delay:?}");
        assert!(delay <= Duration::from_secs(40), "{delay:?}");
        // 到 retry_at 即 due，即使未到下一钟面刻度
        assert!(tr.due(five, at));
        assert!(!tr.due(five, at - Duration::from_secs(1)));

        // 封顶：周期很短时退避不会越过下一刻度
        let ten = Duration::from_secs(10);
        let mut short = tier(Some(t(3800)));
        short.backoff(ten, t(3801));
        let d = short.retry_at.unwrap().duration_since(t(3801)).unwrap();
        assert!(d <= Duration::from_secs(10), "{d:?}");
    }

    #[test]
    fn backoff_escalates_then_advance_resets() {
        let hour = Duration::from_secs(3600);
        let mut tr = tier(Some(t(0)));
        tr.backoff(hour, t(10));
        let first = tr.retry_at.unwrap().duration_since(t(10)).unwrap();
        tr.backoff(hour, t(50));
        let second = tr.retry_at.unwrap().duration_since(t(50)).unwrap();
        assert!(second >= Duration::from_secs(60), "{second:?}");
        assert!(first < second);
        tr.advance(t(100));
        assert_eq!(tr.retry_at, None);
        assert_eq!(tr.retry_level, 0);
        assert_eq!(tr.last_ok, Some(t(100)));
    }

    #[test]
    fn select_jobs_merges_due_and_explicit() {
        let dests = vec![
            Destination::new("http://a", "ta"),
            Destination::new("http://b", "tb"),
            Destination::new("http://c", "tc"),
        ];
        let jobs = vec![SyncJob {
            url: Some("http://b".into()),
            full: true,
        }];
        // 调度到期 + 针对 b 的全量 job：a、c 增量照跑，b 全量，一轮完成
        let out = select_jobs(&dests, &jobs, true, &HashSet::new());
        assert_eq!(out.len(), 3);
        let b = out.iter().find(|j| j.dest.url == "http://b").unwrap();
        assert!(b.full);
        assert!(out
            .iter()
            .filter(|j| j.dest.url != "http://b")
            .all(|j| !j.full));

        // 未到期时只跑 job 目标
        let only_job = select_jobs(&dests, &jobs, false, &HashSet::new());
        assert_eq!(only_job.len(), 1);
        assert_eq!(only_job[0].dest.url, "http://b");
    }

    #[test]
    fn select_jobs_skips_auth_blocked_unless_explicit() {
        let dests = vec![
            Destination::new("http://a", "ta"),
            Destination::new("http://b", "tb"),
        ];
        let blocked: HashSet<String> = ["http://b".to_string()].into();
        // 纯调度轮：封锁目标被跳过
        let out = select_jobs(&dests, &[], true, &blocked);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].dest.url, "http://a");
        // 显式 job 穿透封锁
        let jobs = vec![SyncJob {
            url: Some("http://b".into()),
            full: false,
        }];
        let out = select_jobs(&dests, &jobs, true, &blocked);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn tier_views_reflect_skipped_sources_and_dest_errors() {
        let report = SyncReport {
            sources: vec![
                SourceReport {
                    source: "codex".into(),
                    buckets: 1,
                    sessions: 0,
                    skipped: false,
                    warnings: vec![],
                },
                SourceReport {
                    source: "cursor".into(),
                    buckets: 0,
                    sessions: 0,
                    skipped: true,
                    warnings: vec!["Cursor 登录失效".into()],
                },
            ],
            dests: vec![DestReport {
                url: "http://a".into(),
                full: false,
                ok: true,
                error: None,
                error_kind: None,
                ingested: 1,
                sessions: 0,
                changed_buckets: 1,
                changed_sessions: 0,
                protected: 0,
                dropped: 0,
            }],
        };
        let (agent, cursor) = tier_views(&report, true, true);
        assert!(agent.unwrap().error.is_none(), "本地档不受 cursor skip 影响");
        let cur = cursor.unwrap();
        assert!(cur.error.unwrap().contains("Cursor 登录失效"));

        // 目标失败两档共担
        let mut failed = report.clone();
        failed.dests[0].ok = false;
        failed.dests[0].error = Some("超时".into());
        failed.dests[0].error_kind = Some(PushErrorKind::Retryable);
        let (agent, _) = tier_views(&failed, true, false);
        assert!(agent.unwrap().error.unwrap().contains("超时"));
    }
}
