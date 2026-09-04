mod config;
mod cursor_accounts;
mod cursor_credits;
mod daemon;
mod panel;
mod state;
mod sync;
mod xdg;

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

use config::AgentConfig;

#[derive(Parser)]
#[command(
    name = "ai-usage-agent",
    version,
    about = "采集本机 AI 工具用量并上报看板"
)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 写入或合并配置：已有配置时按 URL upsert 该看板地址，其余字段保留
    Init {
        #[arg(long)]
        url: String,
        #[arg(long)]
        token: String,
        /// 主机显示名（仅在新建或显式传入时写入）
        #[arg(long)]
        hostname: Option<String>,
        #[arg(long, default_value_t = true)]
        upload_project: bool,
        /// 丢弃已有配置整文件重写（旧行为）
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        no_sync: bool,
    },
    /// 解析本地日志并上报（默认增量、全部看板地址）
    Sync {
        /// 忽略增量 state，全量重传
        #[arg(long)]
        full: bool,
        /// 只同步这一个看板地址
        #[arg(long)]
        url: Option<String>,
    },
    /// 显示配置与探测到的工具
    Status,
    /// 前台循环同步并打开本机面板，或安装/卸载 user daemon
    Daemon {
        #[command(subcommand)]
        cmd: Option<DaemonCmd>,
        /// 覆盖配置中的两档间隔（不写回）
        #[arg(long)]
        interval: Option<String>,
    },
}

#[derive(Subcommand)]
enum DaemonCmd {
    Install,
    Uninstall,
    Status,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let config_path = config::resolve_config_path(cli.config.as_ref());
    let data_dir = config::resolve_data_dir(cli.data_dir.as_ref());
    match cli.cmd {
        Commands::Init {
            url,
            token,
            hostname,
            upload_project,
            replace,
            no_sync,
        } => {
            let dest = config::Destination::new(url, token);
            // 已有配置默认 merge；文件损坏时报错而不是悄悄覆盖（--replace 才重写）
            let existing = if replace || !config_path.exists() {
                None
            } else {
                Some(AgentConfig::load_or_setup(&config_path)?)
            };
            let (cfg, dest_added, token_changed) = match existing {
                Some(mut cfg) => {
                    // merge：只 upsert 这一条地址，间隔 / bind / 其余地址保留
                    let old_token = cfg.find_dest(&dest.url).map(|d| d.token);
                    let added = cfg.upsert_destination(dest.clone());
                    let changed = matches!(&old_token, Some(t) if *t != dest.token);
                    if let Some(h) = hostname {
                        cfg.hostname = h;
                    }
                    (cfg, added, changed)
                }
                None => {
                    let hostname = hostname.unwrap_or_else(config::default_hostname);
                    (
                        AgentConfig::new(dest.url.clone(), dest.token, hostname, upload_project),
                        true,
                        false,
                    )
                }
            };
            cfg.save(&config_path)?;
            std::fs::create_dir_all(&data_dir)?;
            println!(
                "已写入 {}（{}）",
                config_path.display(),
                if replace {
                    "整文件重写"
                } else if dest_added {
                    "新增看板地址"
                } else {
                    "更新已有地址 token"
                }
            );
            println!(
                "看板: {}   主机显示名: {}",
                cfg.destinations()
                    .into_iter()
                    .map(|d| d.url)
                    .collect::<Vec<_>>()
                    .join(", "),
                cfg.hostname
            );
            if !no_sync {
                // 新地址或换 token（= 新 host_id）走全量，否则增量
                let full = dest_added || token_changed;
                let jobs = sync::dest_jobs_for_url(&cfg, &dest.url, full)?;
                do_sync_jobs(&cfg, &data_dir, &jobs)?;
            }
        }
        Commands::Sync { full, url } => {
            let cfg = AgentConfig::load(&config_path)?;
            let jobs = match url {
                Some(u) => sync::dest_jobs_for_url(&cfg, &u, full)?,
                None => {
                    let mut jobs = sync::all_dest_jobs(&cfg);
                    for j in &mut jobs {
                        j.full = full;
                    }
                    jobs
                }
            };
            do_sync_jobs(&cfg, &data_dir, &jobs)?;
        }
        Commands::Status => {
            status(&config_path, &data_dir)?;
        }
        Commands::Daemon { cmd, interval } => match cmd {
            None => {
                // setup 模式：配置缺失也能起面板，等待用户在面板补首个看板地址
                let cfg = AgentConfig::load_or_setup(&config_path)?;
                if cfg.destinations().is_empty() {
                    println!("尚未配置看板地址：打开面板填入看板 URL 与 ingest token 即可开始上报。");
                }
                let r#override = match interval {
                    Some(raw) => Some(config::validate_interval(&raw)?),
                    None => None,
                };
                daemon::run_loop(cfg, &config_path, &data_dir, r#override)?;
            }
            Some(DaemonCmd::Install) => {
                let exe = std::env::current_exe()?;
                daemon::install(&exe, &config_path, &data_dir)?;
            }
            Some(DaemonCmd::Uninstall) => daemon::uninstall()?,
            Some(DaemonCmd::Status) => daemon::status()?,
        },
    }
    Ok(())
}

fn do_sync_jobs(cfg: &AgentConfig, data_dir: &Path, jobs: &[sync::DestJob]) -> Result<()> {
    let report = sync::run_sync_jobs(cfg, data_dir, false, None, jobs)?;
    for w in report.warnings() {
        eprintln!("  {w}");
    }
    for line in report.parser_lines() {
        eprintln!("  {line}");
    }
    if report.changed() > 0 {
        println!(
            "已同步 {} buckets · {} sessions",
            report.ingested(),
            report.sessions_total()
        );
        if report.protected() > 0 {
            println!("看板保留了 {} 个更大的已有 bucket", report.protected());
        }
    }
    let errors = report.dest_errors();
    if !errors.is_empty() {
        anyhow::bail!("{}", errors.join("; "));
    }
    Ok(())
}

fn status(config_path: &Path, data_dir: &Path) -> Result<()> {
    println!("config: {}", config_path.display());
    println!("data:   {}", data_dir.display());
    match AgentConfig::load(config_path) {
        Ok(cfg) => {
            for (i, dest) in cfg.destinations().into_iter().enumerate() {
                let label = if i == 0 { "url" } else { "url+" };
                println!("{label:<9} {}", dest.url);
                println!(
                    "token:    {}…",
                    dest.token.chars().take(12).collect::<String>()
                );
                let st = state::SyncState::load(&config::dest_state_path(data_dir, &dest.url));
                println!(
                    "state:    {} buckets · {} sessions",
                    st.buckets.len(),
                    st.sessions.len()
                );
            }
            println!("hostname: {}", cfg.hostname);
            println!(
                "interval: 本地 {} · Cursor {}",
                cfg.interval_local, cfg.interval_cursor
            );
            println!(
                "project:  {}",
                if cfg.upload_project {
                    "上报项目路径"
                } else {
                    "不上报（project=unknown）"
                }
            );
            println!("panel:    http://{}", cfg.bind);
        }
        Err(err) => println!("配置不可用: {err:#}"),
    }
    let extras = cursor_accounts::load(data_dir).unwrap_or_default();
    if extras.accounts.is_empty() {
        println!("cursor extra accounts: (none)");
    } else {
        println!("cursor extra accounts:");
        for a in cursor_accounts::public_views(&extras) {
            let exp = a
                .exp
                .map(|e| format!("exp={e}"))
                .unwrap_or_else(|| "exp=?".into());
            println!("  {}  {}  {exp}", a.account_label, a.account_hash);
        }
    }
    let home = xdg::home_dir();
    let ctx = ai_usage_parsers::ParseCtx {
        home,
        cache_dir: data_dir.join("cache"),
        env: ai_usage_parsers::AdapterEnv {
            cursor_extra_accounts: cursor_accounts::to_env(&extras),
            ..ai_usage_parsers::AdapterEnv::default()
        },
    };
    for adapter in ai_usage_parsers::default_adapters() {
        let dirs = adapter.detect(&ctx);
        if dirs.is_empty() {
            println!("{:<14} 未发现", adapter.id());
        } else {
            println!(
                "{:<14} {}",
                adapter.id(),
                dirs.iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    Ok(())
}
