mod agg;
mod cache;
mod claude;
mod codex;
mod cursor;
mod grok;
mod util;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ai_usage_protocol::{UsageBucket, UsageSession};

pub use claude::ClaudeCodeAdapter;
pub use codex::CodexAdapter;
pub use cursor::{
    extract_cursor_previews, fetch_plan_snapshot, fetch_plan_with_raw, preview_cursor_token,
    read_ide_cursor_auth, snapshot_from_usage_json, CursorAccountSnapshot, CursorAdapter,
    CursorTokenPreview,
};
pub use grok::GrokAdapter;

#[derive(Debug, Clone, Default)]
pub struct CursorExtraAccount {
    pub access_token: String,
    pub account_label: String,
}

#[derive(Debug, Clone, Default)]
pub struct AdapterEnv {
    pub claude_config_dir: Option<PathBuf>,
    pub claude_dirs: Vec<PathBuf>,
    pub codex_home: Option<PathBuf>,
    pub extra_codex_home: Option<PathBuf>,
    pub grok_home: Option<PathBuf>,
    pub cursor_state_db: Option<PathBuf>,
    pub cursor_extra_accounts: Vec<CursorExtraAccount>,
}

#[derive(Debug, Clone)]
pub struct ParseCtx {
    pub home: PathBuf,
    pub cache_dir: PathBuf,
    pub env: AdapterEnv,
}

impl ParseCtx {
    pub fn new(home: PathBuf, cache_dir: PathBuf) -> Self {
        Self {
            home,
            cache_dir,
            env: AdapterEnv::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ParseResult {
    pub buckets: Vec<UsageBucket>,
    pub sessions: Vec<UsageSession>,
    pub skipped: bool,
    pub warnings: Vec<String>,
    /// Account hashes this source attempted (Cursor). Empty → source-level prune.
    pub attempted_account_hashes: Vec<String>,
    pub succeeded_account_hashes: Vec<String>,
}

pub trait UsageAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn detect(&self, ctx: &ParseCtx) -> Vec<PathBuf>;
    fn parse(&self, ctx: &ParseCtx) -> ParseResult;
}

pub fn default_adapters() -> Vec<Box<dyn UsageAdapter>> {
    vec![
        Box::new(ClaudeCodeAdapter),
        Box::new(CodexAdapter),
        Box::new(GrokAdapter),
        Box::new(CursorAdapter),
    ]
}

/// Run adapters with a concurrency cap. A panicking adapter is marked skipped.
pub fn parse_all(ctx: &ParseCtx, limit: usize) -> Vec<(String, ParseResult)> {
    let adapters = default_adapters();
    parse_adapters(ctx, &adapters, limit)
}

pub fn parse_adapters(
    ctx: &ParseCtx,
    adapters: &[Box<dyn UsageAdapter>],
    limit: usize,
) -> Vec<(String, ParseResult)> {
    if adapters.is_empty() {
        return Vec::new();
    }
    let limit = limit.max(1).min(adapters.len());
    let queue = Arc::new(Mutex::new((0usize, adapters.len())));
    let out = Arc::new(Mutex::new(vec![None; adapters.len()]));
    std::thread::scope(|scope| {
        for _ in 0..limit {
            let queue = Arc::clone(&queue);
            let out = Arc::clone(&out);
            scope.spawn(move || loop {
                let idx = {
                    let mut g = queue.lock().unwrap();
                    if g.0 >= g.1 {
                        None
                    } else {
                        let i = g.0;
                        g.0 += 1;
                        Some(i)
                    }
                };
                let Some(idx) = idx else { break };
                let adapter = adapters[idx].as_ref();
                let id = adapter.id().to_string();
                let parsed = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if adapter.detect(ctx).is_empty() {
                        ParseResult::default()
                    } else {
                        adapter.parse(ctx)
                    }
                })) {
                    Ok(r) => r,
                    Err(_) => ParseResult {
                        skipped: true,
                        warnings: vec![format!("{id}: parser panicked")],
                        ..ParseResult::default()
                    },
                };
                out.lock().unwrap()[idx] = Some((id, parsed));
            });
        }
    });
    Arc::try_unwrap(out)
        .expect("workers finished")
        .into_inner()
        .unwrap()
        .into_iter()
        .flatten()
        .collect()
}
