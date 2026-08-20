mod config;
mod daemon;
mod state;
mod sync;
mod xdg;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};

use config::AgentConfig;

#[derive(Parser)]
#[command(name = "ai-usage-agent", version, about = "采集本机 AI 工具用量并上报看板")]
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
    /// 写入配置（URL + ingest token）
    Init {
        #[arg(long)]
        url: String,
        #[arg(long)]
        token: String,
        #[arg(long)]
        hostname: Option<String>,
        #[arg(long, default_value_t = true)]
        upload_project: bool,
        #[arg(long)]
        no_sync: bool,
    },
    /// 解析本地日志并增量上报
    Sync,
    /// 显示配置与探测到的工具
    Status,
    /// 前台循环同步，或安装/卸载 user daemon
    Daemon {
        #[command(subcommand)]
        cmd: Option<DaemonCmd>,
        #[arg(long, default_value = "30m")]
        interval: String,
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
            no_sync,
        } => {
            let hostname = hostname.unwrap_or_else(config::default_hostname);
            let cfg = AgentConfig {
                url: url.trim_end_matches('/').to_string(),
                token,
                hostname,
                upload_project,
            };
            cfg.save(&config_path)?;
            std::fs::create_dir_all(&data_dir)?;
            println!("已写入 {}", config_path.display());
            println!("看板: {}   主机显示名: {}", cfg.url, cfg.hostname);
            if !no_sync {
                do_sync(&cfg, &data_dir)?;
            }
        }
        Commands::Sync => {
            let cfg = AgentConfig::load(&config_path)?;
            do_sync(&cfg, &data_dir)?;
        }
        Commands::Status => {
            status(&config_path, &data_dir)?;
        }
        Commands::Daemon { cmd, interval } => match cmd {
            None => {
                let cfg = AgentConfig::load(&config_path)?;
                let dur = parse_interval(&interval)?;
                println!("daemon 每 {interval} 同步一次，Ctrl+C 结束");
                daemon::run_loop(&cfg, &data_dir, dur)?;
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

fn do_sync(cfg: &AgentConfig, data_dir: &PathBuf) -> Result<()> {
    let report = sync::run_sync(cfg, data_dir, false)?;
    for w in &report.warnings {
        eprintln!("  {w}");
    }
    for line in &report.parser_lines {
        eprintln!("  {line}");
    }
    if report.changed_buckets + report.changed_sessions > 0 {
        println!(
            "已同步 {} buckets · {} sessions",
            report.ingested, report.sessions
        );
        if report.protected > 0 {
            println!("看板保留了 {} 个更大的已有 bucket", report.protected);
        }
    }
    Ok(())
}

fn status(config_path: &PathBuf, data_dir: &PathBuf) -> Result<()> {
    println!("config: {}", config_path.display());
    println!("data:   {}", data_dir.display());
    match AgentConfig::load(config_path) {
        Ok(cfg) => {
            println!("url:      {}", cfg.url);
            println!("hostname: {}", cfg.hostname);
            println!("token:    {}…", cfg.token.chars().take(12).collect::<String>());
        }
        Err(_) => println!("尚未 init"),
    }
    let home = xdg::home_dir();
    let ctx = ai_usage_parsers::ParseCtx::new(home, data_dir.join("cache"));
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

fn parse_interval(s: &str) -> Result<Duration> {
    let s = s.trim();
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
