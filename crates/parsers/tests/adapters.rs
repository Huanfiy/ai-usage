use std::path::PathBuf;

use ai_usage_parsers::{
    parse_all, ClaudeCodeAdapter, CodexAdapter, GrokAdapter, ParseCtx, UsageAdapter,
};
use ai_usage_protocol::session_hash_from_id;

fn fixture_home() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/home")
        .canonicalize()
        .expect("fixtures")
}

fn fixture_ctx(tmp: &tempfile::TempDir) -> ParseCtx {
    ParseCtx::new(fixture_home(), tmp.path().to_path_buf())
}

#[test]
fn claude_dedupes_and_splits_cache_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = fixture_ctx(&tmp);
    let adapter = ClaudeCodeAdapter;
    assert!(!adapter.detect(&ctx).is_empty());
    let result = adapter.parse(&ctx);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert_eq!(result.buckets.len(), 1);
    let b = &result.buckets[0];
    assert_eq!(b.source, "claude-code");
    assert_eq!(b.model, "claude-opus-5");
    assert_eq!(b.project, "demo");
    assert_eq!(b.input_tokens, 15);
    assert_eq!(b.output_tokens, 33);
    assert_eq!(b.cache_creation_input_tokens, 100);
    assert_eq!(b.cache_read_input_tokens, 250);
    assert_eq!(result.sessions.len(), 1);
    assert_eq!(result.sessions[0].user_message_count, 2);
}

#[test]
fn codex_skips_duplicate_token_count() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = fixture_ctx(&tmp);
    let result = CodexAdapter.parse(&ctx);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert_eq!(result.buckets.len(), 1);
    let b = &result.buckets[0];
    assert_eq!(b.source, "codex");
    assert_eq!(b.model, "gpt-5.4");
    assert_eq!(b.project, "demo");
    assert_eq!(b.input_tokens, 80);
    assert_eq!(b.cache_read_input_tokens, 100);
    assert_eq!(b.output_tokens, 30);
    assert_eq!(b.reasoning_output_tokens, 15);
    assert_eq!(b.cache_creation_input_tokens, 0);
}

#[test]
fn codex_replay_files_omit_usage_keep_session() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = fixture_ctx(&tmp);
    let result = CodexAdapter.parse(&ctx);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert_eq!(result.buckets.len(), 1);
    let b = &result.buckets[0];
    assert_eq!(b.input_tokens, 80);
    assert_eq!(b.output_tokens, 30);
    let hashes: Vec<_> = result
        .sessions
        .iter()
        .map(|s| s.session_hash.as_str())
        .collect();
    assert!(hashes.contains(&session_hash_from_id("sess-codex-1").as_str()));
    assert!(hashes.contains(&session_hash_from_id("sess-codex-fork").as_str()));
    assert!(hashes.contains(&session_hash_from_id("sess-codex-sub").as_str()));
}

#[test]
fn grok_splits_model_usage_and_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = fixture_ctx(&tmp);
    let result = GrokAdapter.parse(&ctx);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert_eq!(result.buckets.len(), 1);
    let b = &result.buckets[0];
    assert_eq!(b.source, "grok");
    assert_eq!(b.model, "grok-4.6-build");
    assert_eq!(b.project, "demo");
    assert_eq!(b.input_tokens, 600);
    assert_eq!(b.cache_read_input_tokens, 400);
    assert_eq!(b.cache_creation_input_tokens, 50);
    assert_eq!(b.output_tokens, 60);
    assert_eq!(b.reasoning_output_tokens, 20);
}

#[test]
fn parse_all_runs_three_adapters() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = fixture_ctx(&tmp);
    let results = parse_all(&ctx, 4);
    let ids: Vec<_> = results.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["claude-code", "codex", "grok"]);
    let buckets: i64 = results
        .iter()
        .map(|(_, r)| r.buckets.iter().map(|b| b.total_tokens).sum::<i64>())
        .sum();
    assert!(buckets > 0);
}
