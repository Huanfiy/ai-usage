use std::io::Write;
use std::path::{Path, PathBuf};

use ai_usage_parsers::{parse_adapters, ParseCtx, ParseResult, PiAdapter, UsageAdapter};
use ai_usage_protocol::{session_hash_from_id, UsageBucket, UsageSession};
use serde_json::{json, Value};

fn ctx(tmp: &tempfile::TempDir) -> ParseCtx {
    let mut ctx = ParseCtx::new(tmp.path().join("home"), tmp.path().join("cache"));
    ctx.env.pi_session_dir = Some(ctx.home.join(".pi/agent/sessions"));
    ctx
}

fn path(ctx: &ParseCtx) -> PathBuf {
    ctx.env
        .pi_session_dir
        .as_ref()
        .unwrap()
        .join("project/session.jsonl")
}

fn header(id: &str) -> Value {
    json!({"type":"session", "version":3, "id":id,
        "timestamp":"2026-01-15T10:00:00Z", "cwd":"/private/workspace/demo"})
}

fn user() -> Value {
    json!({"type":"message", "id":"user", "timestamp":"2026-01-15T10:00:00Z",
        "message":{"role":"user", "content":"secret prompt\u{2028}not a new record"}})
}

fn assistant(id: &str, input: i64) -> Value {
    json!({"type":"message", "id":id, "parentId":"user", "timestamp":"2026-01-15T10:01:00Z",
        "message":{"role":"assistant", "provider":"openai", "model":"gpt-5.4",
        "stopReason":"stop", "usage":{"input":input, "output":0},
        "content":[{"type":"text", "text":"secret completion"}]}})
}

fn encode(entries: &[Value]) -> Vec<u8> {
    entries
        .iter()
        .flat_map(|e| {
            let mut line = serde_json::to_vec(e).unwrap();
            line.push(b'\n');
            line
        })
        .collect()
}

fn write_log(path: &Path, entries: &[Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, encode(entries)).unwrap();
}

fn append(path: &Path, bytes: &[u8]) {
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

fn parse(ctx: &ParseCtx) -> ParseResult {
    let result = PiAdapter.parse(ctx);
    assert!(!result.skipped);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    result
}

fn total(result: &ParseResult) -> i64 {
    result.buckets.iter().map(|b| b.total_tokens).sum()
}

fn snapshot(mut result: ParseResult) -> (Vec<UsageBucket>, Vec<UsageSession>) {
    result.buckets.sort_by_key(UsageBucket::client_key);
    result.sessions.sort_by_key(UsageSession::client_key);
    (result.buckets, result.sessions)
}

fn cache_file(ctx: &ParseCtx) -> PathBuf {
    let files: Vec<_> = std::fs::read_dir(ctx.cache_dir.join("pi"))
        .unwrap()
        .collect();
    assert_eq!(files.len(), 1);
    files[0].as_ref().unwrap().path()
}

#[test]
fn fixture_covers_models_cache_reasoning_branches_summaries_and_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let mut ctx = ctx(&tmp);
    ctx.home = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/home");
    ctx.env.pi_session_dir = Some(ctx.home.join(".pi/agent/sessions"));
    assert_eq!(PiAdapter.id(), "pi");
    assert_eq!(PiAdapter.detect(&ctx).len(), 1);
    let r = parse(&ctx);
    assert_eq!(r.buckets.len(), 3);
    assert_eq!(total(&r), 462);
    let gpt = r.buckets.iter().find(|b| b.model == "gpt-5.4").unwrap();
    assert_eq!(gpt.source, "pi");
    assert_eq!(gpt.project, "demo");
    assert_eq!(gpt.bucket_start.to_rfc3339(), "2026-01-15T10:00:00+00:00");
    assert_eq!(gpt.input_tokens, 110);
    assert_eq!(gpt.output_tokens, 35);
    assert_eq!(gpt.cache_read_input_tokens, 203);
    assert_eq!(gpt.cache_creation_input_tokens, 50);
    assert_eq!(gpt.reasoning_output_tokens, 10);
    assert_eq!(gpt.total_tokens, 408);
    let claude = r
        .buckets
        .iter()
        .find(|b| b.model == "claude-sonnet-4-5")
        .unwrap();
    assert_eq!(
        claude.bucket_start.to_rfc3339(),
        "2026-01-15T10:30:00+00:00"
    );
    assert_eq!(claude.total_tokens, 42, "the alternate branch also counts");
    let summaries = r.buckets.iter().find(|b| b.model == "unknown").unwrap();
    assert_eq!(
        summaries.total_tokens, 12,
        "only top-level summary usage counts"
    );
    assert_eq!(
        r.sessions.len(),
        2,
        "the shorter copy must not add a session"
    );
    let main = r
        .sessions
        .iter()
        .find(|s| s.session_hash == session_hash_from_id("pi-main"))
        .unwrap();
    assert_eq!(main.total_tokens, total(&r));
    assert_eq!(main.message_count, 7);
    assert_eq!(main.user_message_count, 2);
    assert_eq!(main.duration_seconds, 35 * 60);
    assert_eq!(main.active_seconds, 5 * 60);
    let fork = r
        .sessions
        .iter()
        .find(|s| s.session_hash == session_hash_from_id("pi-fork"))
        .unwrap();
    assert_eq!(
        fork.total_tokens, 0,
        "copied history AND new fork requests are excluded"
    );
    assert_eq!(fork.message_count, 4);
    assert_eq!(snapshot(r), snapshot(parse(&ctx)));
}

#[test]
fn error_and_aborted_usage_count_but_pending_bridge_zero_and_missing_do_not() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(&tmp);
    let mut entries = vec![header("s"), user()];
    for reason in ["stop", "toolUse", "length", "error", "aborted", "pending"] {
        let mut a = assistant(reason, 10);
        a["message"]["stopReason"] = json!(reason);
        entries.push(a);
    }
    entries.push(assistant("zero", 0));
    let mut missing = assistant("missing", 99);
    missing["message"].as_object_mut().unwrap().remove("usage");
    entries.push(missing);
    let mut bridge = assistant("bridge", 999);
    bridge["message"]["provider"] = json!("cursor-agent");
    entries.push(bridge);
    write_log(&path(&ctx), &entries);
    let r = parse(&ctx);
    assert_eq!(total(&r), 50);
    assert_eq!(r.sessions[0].message_count, 10);
    assert_eq!(r.sessions[0].user_message_count, 1);
}

#[test]
fn bridge_only_session_still_has_timing_and_no_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(&tmp);
    let mut a = assistant("a", 999);
    a["message"]["provider"] = json!("cursor-agent");
    write_log(&path(&ctx), &[header("bridge"), user(), a]);
    let r = parse(&ctx);
    assert!(r.buckets.is_empty());
    assert_eq!(r.sessions.len(), 1);
    assert_eq!(r.sessions[0].total_tokens, 0);
    assert_eq!(r.sessions[0].duration_seconds, 60);
    assert_eq!(r.sessions[0].message_count, 2);
}

#[test]
fn normalizes_token_subsets_and_falls_back_to_message_milliseconds() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(&tmp);
    let mut a = assistant("a", 0);
    a.as_object_mut().unwrap().remove("timestamp");
    a["message"]["timestamp"] = json!(chrono::DateTime::parse_from_rfc3339(
        "2026-01-15T18:31:00+08:00"
    )
    .unwrap()
    .timestamp_millis());
    a["message"]["usage"] = json!({"input":-5, "output":5, "reasoning":99,
        "cacheRead":4, "cacheWrite":2, "cacheWrite1h":2, "totalTokens":999999,
        "cost":{"total":999999}});
    a["message"]["responseModel"] = json!(" ");
    write_log(&path(&ctx), &[header("s"), user(), a]);
    let r = parse(&ctx);
    let b = &r.buckets[0];
    assert_eq!(b.input_tokens, 0);
    assert_eq!(b.output_tokens, 0);
    assert_eq!(b.reasoning_output_tokens, 5);
    assert_eq!(b.cache_read_input_tokens, 4);
    assert_eq!(b.cache_creation_input_tokens, 2);
    assert_eq!(b.total_tokens, 11);
    assert_eq!(b.model, "gpt-5.4");
    assert_eq!(b.bucket_start.to_rfc3339(), "2026-01-15T10:30:00+00:00");
}

#[test]
fn incomplete_tail_retries_without_double_counting_and_cache_matches_cold_parse() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(&tmp);
    let path = path(&ctx);
    write_log(&path, &[header("s"), user(), assistant("a", 10)]);
    let initial_size = std::fs::metadata(&path).unwrap().len();
    assert_eq!(total(&parse(&ctx)), 10);
    let next = serde_json::to_vec(&assistant("b", 4)).unwrap();
    let split = next.len() / 2;
    append(&path, &next[..split]);
    assert_eq!(total(&parse(&ctx)), 10);
    let cached: Value = serde_json::from_slice(&std::fs::read(cache_file(&ctx)).unwrap()).unwrap();
    assert_eq!(cached["parsed_bytes"], initial_size);
    append(&path, &next[split..]);
    assert_eq!(
        total(&parse(&ctx)),
        10,
        "even valid JSON waits for its newline"
    );
    append(&path, b"\n");
    assert_eq!(total(&parse(&ctx)), 14);
    append(&path, &encode(&[assistant("a", 20), assistant("a", 1)]));
    let warm = parse(&ctx);
    assert_eq!(
        total(&warm),
        24,
        "duplicate IDs retain the largest usage, not a sum"
    );
    assert_eq!(warm.sessions[0].message_count, 3);
    let source = std::fs::read(&path).unwrap();
    assert_eq!(snapshot(parse(&ctx)), snapshot(warm));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        source,
        "source files are read-only"
    );
    let mut cold_ctx = ctx.clone();
    cold_ctx.cache_dir = tmp.path().join("cold-cache");
    assert_eq!(snapshot(parse(&ctx)), snapshot(parse(&cold_ctx)));
}

#[test]
fn truncation_replacement_and_rewritten_growing_prefix_invalidate_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(&tmp);
    let path = path(&ctx);
    write_log(&path, &[header("s"), user(), assistant("a", 7)]);
    assert_eq!(total(&parse(&ctx)), 7);
    write_log(&path, &[header("s")]);
    let truncated = parse(&ctx);
    assert!(truncated.buckets.is_empty());
    assert!(truncated.sessions.is_empty());
    write_log(&path, &[header("s"), user(), assistant("a", 7)]);
    assert_eq!(total(&parse(&ctx)), 7);
    // Same inode, larger file, but the old prefix changed: cannot just tail-read.
    write_log(
        &path,
        &[header("s"), user(), assistant("a", 9), assistant("b", 4)],
    );
    assert_eq!(total(&parse(&ctx)), 13);
    let replacement = path.with_extension("replacement");
    write_log(
        &replacement,
        &[header("s"), user(), assistant("a", 1), assistant("b", 4)],
    );
    std::fs::rename(replacement, &path).unwrap();
    assert_eq!(total(&parse(&ctx)), 5);
}

#[test]
fn changed_header_and_cache_version_rebuild_accounting() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(&tmp);
    let path = path(&ctx);
    write_log(&path, &[header("s"), user(), assistant("a", 7)]);
    let initial = snapshot(parse(&ctx));
    let cache_path = cache_file(&ctx);
    let mut cached: Value = serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    cached["algorithm_version"] = json!(0);
    cached["records"] = json!({});
    std::fs::write(&cache_path, serde_json::to_vec(&cached).unwrap()).unwrap();
    assert_eq!(snapshot(parse(&ctx)), initial);
    let mut fork_header = header("s");
    fork_header["parentSession"] = json!("/private/parent.jsonl");
    write_log(
        &path,
        &[fork_header, user(), assistant("a", 7), assistant("b", 5)],
    );
    let fork = parse(&ctx);
    assert!(fork.buckets.is_empty());
    assert_eq!(fork.sessions[0].total_tokens, 0);
}

#[test]
fn cache_contains_only_accounting_metadata_not_conversation_or_parent_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(&tmp);
    let mut h = header("s");
    h["parentSession"] = json!("/secret/parent/session.jsonl");
    write_log(&path(&ctx), &[h, user(), assistant("a", 7)]);
    parse(&ctx);
    let cache = std::fs::read_to_string(cache_file(&ctx)).unwrap();
    for secret in [
        "secret prompt",
        "secret completion",
        "/private/workspace",
        "/secret/parent",
        "\"content\"",
        "\"cost\"",
    ] {
        assert!(!cache.contains(secret), "cached private data: {secret}");
    }
}

#[test]
fn corrupt_complete_line_warns_but_unicode_content_and_valid_records_survive() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(&tmp);
    let path = path(&ctx);
    write_log(&path, &[header("s"), user()]);
    append(&path, b"not-json secret-invalid-record\n");
    append(&path, &encode(&[assistant("a", 7)]));
    let r = PiAdapter.parse(&ctx);
    assert_eq!(total(&r), 7);
    assert_eq!(r.sessions[0].message_count, 2);
    assert_eq!(r.warnings.len(), 1);
    assert!(!r.warnings[0].contains("secret-invalid-record"));
    assert_eq!(r.warnings, PiAdapter.parse(&ctx).warnings);
}

#[test]
fn invalid_headers_fail_closed_and_missing_directory_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(&tmp);
    assert!(PiAdapter.detect(&ctx).is_empty());
    assert!(parse(&ctx).sessions.is_empty());
    let adapters: Vec<Box<dyn UsageAdapter>> = vec![Box::new(PiAdapter)];
    for h in [
        json!({"type":"session","version":3}),
        json!({"type":"session","version":99,"id":"s"}),
        user(),
    ] {
        write_log(&path(&ctx), &[h, assistant("a", 7)]);
        let r = parse_adapters(&ctx, &adapters, 1).remove(0).1;
        assert!(r.skipped, "bad header must preserve sync state");
        assert!(r.buckets.is_empty());
        assert!(!r.warnings.is_empty());
    }
}

#[test]
fn legacy_v1_without_entry_ids_counts_physical_records_and_v2_is_supported() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(&tmp);
    for version in [1, 2] {
        let mut h = header("s");
        h["version"] = json!(version);
        let mut a = assistant("a", 7);
        let mut b = assistant("b", 3);
        if version == 1 {
            h.as_object_mut().unwrap().remove("version");
            a.as_object_mut().unwrap().remove("id");
            b.as_object_mut().unwrap().remove("id");
        }
        write_log(&path(&ctx), &[h, user(), a, b]);
        assert_eq!(total(&parse(&ctx)), 10);
        assert_eq!(total(&parse(&ctx)), 10, "cached reads are identical");
    }
}
