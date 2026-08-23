use std::path::PathBuf;

use ai_usage_parsers::{
    parse_all, AdapterEnv, ClaudeCodeAdapter, CodexAdapter, CursorAdapter, GrokAdapter, ParseCtx,
    UsageAdapter,
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
    assert_eq!(result.sessions[0].input_tokens, 15);
    assert_eq!(result.sessions[0].output_tokens, 33);
    assert_eq!(result.sessions[0].cache_creation_input_tokens, 100);
    assert_eq!(result.sessions[0].cache_read_input_tokens, 250);
    assert_eq!(result.sessions[0].total_tokens, 398);
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
    let main = result
        .sessions
        .iter()
        .find(|s| s.session_hash == session_hash_from_id("sess-codex-1"))
        .unwrap();
    assert_eq!(main.input_tokens, 80);
    assert_eq!(main.cache_read_input_tokens, 100);
    assert_eq!(main.output_tokens, 30);
    assert_eq!(main.reasoning_output_tokens, 15);
    assert_eq!(main.total_tokens, 225);
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
    for s in &result.sessions {
        if s.session_hash == session_hash_from_id("sess-codex-1") {
            assert_eq!(s.total_tokens, 225);
        } else {
            assert_eq!(s.total_tokens, 0);
        }
    }
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
    let main = result
        .sessions
        .iter()
        .find(|s| s.session_hash == session_hash_from_id("sess-1"))
        .unwrap();
    assert_eq!(main.input_tokens, 600);
    assert_eq!(main.cache_read_input_tokens, 400);
    assert_eq!(main.cache_creation_input_tokens, 50);
    assert_eq!(main.output_tokens, 60);
    assert_eq!(main.reasoning_output_tokens, 20);
    assert_eq!(main.total_tokens, 1130);
}

#[test]
fn grok_subagent_omits_usage_keeps_session() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = fixture_ctx(&tmp);
    let result = GrokAdapter.parse(&ctx);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert_eq!(result.buckets.len(), 1);
    let b = &result.buckets[0];
    assert_eq!(b.input_tokens, 600);
    assert_eq!(b.output_tokens, 60);
    let hashes: Vec<_> = result
        .sessions
        .iter()
        .map(|s| s.session_hash.as_str())
        .collect();
    assert!(hashes.contains(&session_hash_from_id("sess-1").as_str()));
    assert!(hashes.contains(&session_hash_from_id("sess-sub").as_str()));
    for s in &result.sessions {
        if s.session_hash == session_hash_from_id("sess-1") {
            assert_eq!(s.total_tokens, 1130);
        } else {
            assert_eq!(s.total_tokens, 0);
        }
    }
}

#[test]
fn parse_all_runs_registered_adapters() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = fixture_ctx(&tmp);
    let results = parse_all(&ctx, 4);
    let ids: Vec<_> = results.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["claude-code", "codex", "grok", "cursor"]);
    let buckets: i64 = results
        .iter()
        .map(|(_, r)| r.buckets.iter().map(|b| b.total_tokens).sum::<i64>())
        .sum();
    assert!(buckets > 0);
    let cursor = results.iter().find(|(id, _)| id == "cursor").unwrap();
    assert!(!cursor.1.skipped);
    assert!(cursor.1.buckets.is_empty());
}

#[test]
fn cursor_detects_explicit_state_db() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("state.vscdb");
    std::fs::write(&db, b"").unwrap();
    let ctx = ParseCtx {
        home: fixture_home(),
        cache_dir: tmp.path().join("cache"),
        env: AdapterEnv {
            cursor_state_db: Some(db.clone()),
            ..AdapterEnv::default()
        },
    };
    assert_eq!(CursorAdapter.detect(&ctx), vec![db]);
}
