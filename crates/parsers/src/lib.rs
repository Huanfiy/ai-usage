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
    CursorTokenPreview, PlanFetchError,
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
                    Ok(r) => fail_closed(&id, r),
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

/// Root detected but nothing parsed and errors occurred → mark skipped so the
/// agent keeps its incremental state (a transient failure must not turn into a
/// full re-upload after recovery). Account-scoped sources manage their own
/// skip and per-account prune sets, so they are exempt.
fn fail_closed(id: &str, mut r: ParseResult) -> ParseResult {
    if !r.skipped
        && r.attempted_account_hashes.is_empty()
        && r.buckets.is_empty()
        && r.sessions.is_empty()
        && !r.warnings.is_empty()
    {
        r.skipped = true;
        r.warnings.push(format!("{id}: 本轮无成功解析，保留增量 state 不修剪"));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeAdapter {
        result: ParseResult,
    }

    impl UsageAdapter for FakeAdapter {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn detect(&self, _ctx: &ParseCtx) -> Vec<PathBuf> {
            vec![PathBuf::from("/tmp/fake")]
        }
        fn parse(&self, _ctx: &ParseCtx) -> ParseResult {
            self.result.clone()
        }
    }

    fn ctx() -> ParseCtx {
        ParseCtx::new(PathBuf::from("/nonexistent"), PathBuf::from("/nonexistent"))
    }

    #[test]
    fn all_failed_local_source_is_fail_closed() {
        let adapters: Vec<Box<dyn UsageAdapter>> = vec![Box::new(FakeAdapter {
            result: ParseResult {
                warnings: vec!["fake: cannot read x".into()],
                ..ParseResult::default()
            },
        })];
        let out = parse_adapters(&ctx(), &adapters, 1);
        assert_eq!(out.len(), 1);
        assert!(out[0].1.skipped, "root exists + all failed → skipped");
    }

    #[test]
    fn empty_without_warnings_stays_prunable() {
        let adapters: Vec<Box<dyn UsageAdapter>> = vec![Box::new(FakeAdapter {
            result: ParseResult::default(),
        })];
        let out = parse_adapters(&ctx(), &adapters, 1);
        assert!(!out[0].1.skipped, "no logs at all → normal prune path");
    }

    #[test]
    fn account_scoped_source_is_exempt() {
        let adapters: Vec<Box<dyn UsageAdapter>> = vec![Box::new(FakeAdapter {
            result: ParseResult {
                warnings: vec!["one account failed".into()],
                attempted_account_hashes: vec!["a".into()],
                succeeded_account_hashes: vec![],
                ..ParseResult::default()
            },
        })];
        let out = parse_adapters(&ctx(), &adapters, 1);
        assert!(!out[0].1.skipped, "account prune sets govern instead");
    }
}
