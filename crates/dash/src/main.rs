mod config;
mod db;
mod http;
mod ingest;
mod paths;
mod pricing;
mod query;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use config::DashConfig;
use db::Db;
use http::AppState;
use pricing::PriceBook;

#[derive(Parser)]
#[command(
    name = "ai-usage-dash",
    version,
    about = "AI 用量看板：接收上报、聚合、展示"
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
    /// 启动看板服务
    Serve {
        #[arg(long)]
        bind: Option<String>,
        /// 绑定非回环地址且未设置 ui_token 时，确认前面有反向代理
        #[arg(long)]
        behind_proxy: bool,
        #[arg(long)]
        pricing_override: Option<PathBuf>,
    },
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    Pricing {
        #[command(subcommand)]
        cmd: PricingCmd,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    Create {
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        hostname: Option<String>,
    },
    List,
    Revoke {
        host_id: String,
    },
}

#[derive(Subcommand)]
enum PricingCmd {
    Update,
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let config_path = config::resolve_config(cli.config.as_ref());
    let data_dir = config::resolve_data(cli.data_dir.as_ref());
    let mut cfg = DashConfig::load_or_default(&config_path)?;
    let db = Db::open(&data_dir.join("usage.sqlite"))?;

    match cli.cmd {
        Commands::Serve {
            bind,
            behind_proxy,
            pricing_override,
        } => {
            if let Some(b) = bind {
                cfg.bind = b;
            }
            if !cfg.is_loopback_bind() && cfg.ui_token.is_empty() && !behind_proxy {
                bail!(
                    "绑定 {} 需要设置 ui_token（dash.toml）或加 --behind-proxy",
                    cfg.bind
                );
            }
            if !config_path.exists() {
                cfg.save(&config_path)?;
            }
            let book = PriceBook::load(&data_dir, pricing_override.as_deref())?;
            if let Some((token, host_id)) = http::bootstrap_token_if_empty(&db)? {
                println!("已创建本机 ingest token（只显示一次）:");
                println!("  token:   {token}");
                println!("  host_id: {host_id}");
                println!(
                    "采集端: ai-usage-agent init --url http://{} --token {token}",
                    cfg.bind
                );
            }
            let state = AppState {
                db: Arc::new(db),
                pricing: Arc::new(RwLock::new(book)),
                config: cfg.clone(),
            };
            let app = http::router(state).layer(tower_http::trace::TraceLayer::new_for_http());
            let addr = cfg.bind_addr()?;
            let listener = tokio::net::TcpListener::bind(addr).await?;
            println!("看板 http://{addr}");
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await?;
        }
        Commands::Token { cmd } => match cmd {
            TokenCmd::Create { label, hostname } => {
                let token = http::new_token();
                let hash = ai_usage_protocol::hash_token(&token);
                let host_id = ai_usage_protocol::host_id_from_token(&token);
                let prefix: String = token.chars().take(12).collect();
                db.with(|c| {
                    db::insert_token(
                        c,
                        &hash,
                        &host_id,
                        &prefix,
                        label.as_deref(),
                        hostname.as_deref().unwrap_or("unnamed"),
                    )
                })?;
                println!("token:   {token}");
                println!("host_id: {host_id}");
            }
            TokenCmd::List => {
                let items = db.with(db::list_tokens)?;
                println!("{}", serde_json::to_string_pretty(&items)?);
            }
            TokenCmd::Revoke { host_id } => {
                let ok = db.with(|c| db::revoke_token(c, &host_id))?;
                if ok {
                    println!("已吊销 {host_id}");
                } else {
                    bail!("未找到可吊销的 token");
                }
            }
        },
        Commands::Pricing { cmd } => match cmd {
            PricingCmd::Update => {
                let n = pricing::fetch_and_store(&data_dir)?;
                println!(
                    "已更新 {n} 条模型报价 → {}",
                    data_dir.join("pricing.json").display()
                );
            }
        },
    }
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
