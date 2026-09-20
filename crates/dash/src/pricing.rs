use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const SNAPSHOT: &[u8] = include_bytes!("../pricing/litellm-snapshot.json");
/// Cursor first-party list prices. Not refreshed by `pricing update`.
const CURSOR_MODELS: &[u8] = include_bytes!("../pricing/cursor-models.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceBookFile {
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub models: HashMap<String, ModelPrice>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelPrice {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
    #[serde(default)]
    pub reasoning: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct PriceBook {
    pub updated_at: Option<String>,
    /// Data-dir `pricing.json` was newer than the embedded snapshot and is in effect.
    pub cache_applied: bool,
    index: HashMap<String, ModelPrice>,
}

impl PriceBook {
    pub fn load(data_dir: &Path, override_path: Option<&Path>) -> Result<Self> {
        let snapshot: PriceBookFile =
            serde_json::from_slice(SNAPSHOT).context("嵌入报价快照损坏")?;
        let mut merged = snapshot.models;
        let mut updated_at = snapshot.updated_at;
        // A cache older than the snapshot is left alone: otherwise one click of
        // "更新价目表" would pin a host to that table across every later release.
        let cache = read_cache(&data_dir.join("pricing.json"))
            .filter(|c| !older_than(c.updated_at.as_deref(), updated_at.as_deref()));
        let cache_applied = cache.is_some();
        if let Some(cache) = cache {
            merged.extend(cache.models);
            if cache.updated_at.is_some() {
                updated_at = cache.updated_at;
            }
        }
        let cursor: PriceBookFile =
            serde_json::from_slice(CURSOR_MODELS).context("嵌入 Cursor 报价损坏")?;
        for (k, v) in cursor.models {
            merged.entry(k).or_insert(v);
        }
        if let Some(over) = override_path {
            if over.is_file() {
                apply_override(&mut merged, over)?;
            }
        }
        let default_over = crate::paths::config_dir().join("pricing.override.json");
        if override_path.is_none() && default_over.is_file() {
            apply_override(&mut merged, &default_over)?;
        }
        Ok(Self {
            updated_at,
            cache_applied,
            index: build_index(merged),
        })
    }

    /// Distinct normalized models carrying a price.
    pub fn model_count(&self) -> usize {
        self.index.len()
    }

    pub fn lookup(&self, model: &str) -> Option<&ModelPrice> {
        let n = normalize_model(model);
        if let Some(p) = self.lookup_exact(&n) {
            return Some(p);
        }
        if let Some(canon) = canonicalize_cursor_slug(&n) {
            if canon != n {
                if let Some(p) = self.lookup_exact(&canon) {
                    return Some(p);
                }
            }
            // Do not prefix-match Fast onto standard (cursor-grok-4.6-xhigh-fast
            // starts with cursor-grok-4.6).
            return None;
        }
        // longest prefix: claude-opus-5-thinking → claude-opus-5
        let mut best: Option<(&String, &ModelPrice)> = None;
        for (k, v) in &self.index {
            if n.starts_with(k) || k.starts_with(&n) {
                if best
                    .as_ref()
                    .map(|(bk, _)| k.len() > bk.len())
                    .unwrap_or(true)
                {
                    best = Some((k, v));
                }
            }
        }
        best.map(|(_, v)| v)
    }

    fn lookup_exact(&self, n: &str) -> Option<&ModelPrice> {
        if let Some(p) = self.index.get(n) {
            return Some(p);
        }
        n.strip_suffix("-build")
            .and_then(|stripped| self.index.get(stripped))
    }

    pub fn cost_usd(&self, model: &str, tokens: TokenSlice) -> Option<f64> {
        let p = self.lookup(model)?;
        let cache_read = p.cache_read.unwrap_or(p.input * 0.1);
        let cache_write = p.cache_write.unwrap_or(p.input * 1.25);
        let reasoning = p.reasoning.unwrap_or(p.output);
        Some(
            tokens.input as f64 * p.input
                + tokens.output as f64 * p.output
                + tokens.cache_read as f64 * cache_read
                + tokens.cache_write as f64 * cache_write
                + tokens.reasoning as f64 * reasoning,
        )
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TokenSlice {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub reasoning: i64,
}

fn read_cache(path: &Path) -> Option<PriceBookFile> {
    let raw = std::fs::read(path).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// True only when both stamps parse and `a` is strictly before `b`.
fn older_than(a: Option<&str>, b: Option<&str>) -> bool {
    let parse = |s: Option<&str>| s.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
    match (parse(a), parse(b)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
}

fn apply_override(merged: &mut HashMap<String, ModelPrice>, path: &Path) -> Result<()> {
    let file: PriceBookFile = serde_json::from_slice(&std::fs::read(path)?)?;
    for (k, v) in file.models {
        merged.insert(k, v);
    }
    Ok(())
}

fn build_index(models: HashMap<String, ModelPrice>) -> HashMap<String, ModelPrice> {
    let mut index: HashMap<String, (u8, ModelPrice)> = HashMap::new();
    for (key, price) in models {
        let n = normalize_model(&key);
        let rank = key_rank(&key);
        match index.get(&n) {
            Some((r, _)) if *r <= rank => {}
            _ => {
                index.insert(n, (rank, price));
            }
        }
    }
    index.into_iter().map(|(k, (_, v))| (k, v)).collect()
}

fn key_rank(key: &str) -> u8 {
    // Prefer canonical unprefixed keys over azure/bedrock copies.
    if key.contains('/') || key.contains('.') {
        2
    } else {
        0
    }
}

/// Cursor CSV slugs include effort (`xhigh`/`high`/…) before the speed tier.
/// Pricing only distinguishes Fast vs standard; do not reuse query display
/// folding (that also strips `cursor-` / `thinking` / `max`).
fn canonicalize_cursor_slug(n: &str) -> Option<String> {
    if !(n.starts_with("composer-") || n.starts_with("cursor-grok-")) {
        return None;
    }
    let parts: Vec<&str> = n
        .split('-')
        .filter(|p| !matches!(*p, "xhigh" | "high" | "medium" | "low"))
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("-"))
}

pub fn normalize_model(name: &str) -> String {
    let mut s = name.trim().to_lowercase();
    const PREFIXES: &[&str] = &[
        "anthropic/",
        "openai/",
        "xai/",
        "google/",
        "azure/",
        "azure_ai/",
        "vertex_ai/",
        "bedrock/",
        "openrouter/",
        "grok/",
    ];
    for p in PREFIXES {
        if let Some(rest) = s.strip_prefix(p) {
            s = rest.to_string();
            break;
        }
    }
    if let Some((_, rest)) = s.split_once('/') {
        s = rest.to_string();
    }
    s
}

pub const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// Downloads the upstream LiteLLM table and caches it in the data dir.
pub fn fetch_and_store(data_dir: &Path) -> Result<usize> {
    let resp = ureq::get(LITELLM_URL)
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let raw: serde_json::Value = resp.into_json()?;
    let file = PriceBookFile {
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
        models: parse_litellm(&raw)?,
    };
    store(data_dir, &file)
}

/// Caches a table handed over as bytes, for a dash host that cannot reach
/// upstream itself: either the raw LiteLLM JSON or the condensed
/// `{updated_at, models}` file `scripts/update-pricing-snapshot.py` writes.
pub fn import_and_store(data_dir: &Path, raw: &[u8]) -> Result<usize> {
    let file = parse_import(raw)?;
    store(data_dir, &file)
}

pub fn parse_import(raw: &[u8]) -> Result<PriceBookFile> {
    let v: serde_json::Value = serde_json::from_slice(raw).context("不是合法 JSON")?;
    let file = if v.get("models").is_some_and(serde_json::Value::is_object) {
        let mut file: PriceBookFile = serde_json::from_value(v).context("精简快照格式不对")?;
        if file.updated_at.is_none() {
            file.updated_at = Some(chrono::Utc::now().to_rfc3339());
        }
        file
    } else {
        PriceBookFile {
            updated_at: Some(chrono::Utc::now().to_rfc3339()),
            models: parse_litellm(&v)?,
        }
    };
    if file.models.is_empty() {
        anyhow::bail!("没有解析到任何模型报价：既不是 LiteLLM 原始价目表，也不是精简快照");
    }
    Ok(file)
}

fn parse_litellm(raw: &serde_json::Value) -> Result<HashMap<String, ModelPrice>> {
    let obj = raw
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("报价 JSON 不是对象"))?;
    let mut models = HashMap::new();
    for (k, v) in obj {
        if !v.is_object() {
            continue;
        }
        let input = v.get("input_cost_per_token").and_then(|x| x.as_f64());
        let output = v.get("output_cost_per_token").and_then(|x| x.as_f64());
        if input.is_none() && output.is_none() {
            continue;
        }
        models.insert(
            k.clone(),
            ModelPrice {
                input: input.unwrap_or(0.0),
                output: output.unwrap_or(0.0),
                cache_read: v
                    .get("cache_read_input_token_cost")
                    .or_else(|| v.get("input_cost_per_token_cache_hit"))
                    .and_then(|x| x.as_f64()),
                cache_write: v
                    .get("cache_creation_input_token_cost")
                    .and_then(|x| x.as_f64()),
                reasoning: v
                    .get("output_cost_per_reasoning_token")
                    .and_then(|x| x.as_f64()),
            },
        );
    }
    Ok(models)
}

fn store(data_dir: &Path, file: &PriceBookFile) -> Result<usize> {
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(
        data_dir.join("pricing.json"),
        serde_json::to_vec_pretty(file)?,
    )?;
    Ok(file.models.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_book() -> PriceBook {
        PriceBook::load(Path::new("/tmp/does-not-exist-ai-usage"), None).unwrap()
    }

    #[test]
    fn finds_canonical_models() {
        let book = empty_book();
        assert!(book.lookup("claude-opus-5").is_some());
        assert!(book.lookup("gpt-5.4").is_some());
        assert!(book.lookup("grok-4.6-build").is_some());
        assert!(book.lookup("xai/grok-4.6").is_some());
    }

    #[test]
    fn cursor_first_party_uses_official_fast_vs_standard() {
        let book = empty_book();
        let composer_fast = book.lookup("composer-2.5-fast").unwrap();
        let composer = book.lookup("composer-2.5").unwrap();
        assert!(composer_fast.input > composer.input);

        let grok_fast = book.lookup("cursor-grok-4.6-xhigh-fast").unwrap();
        let grok_std = book.lookup("cursor-grok-4.6-high").unwrap();
        assert_eq!(
            grok_fast.input,
            book.lookup("cursor-grok-4.6-fast").unwrap().input
        );
        assert_eq!(
            grok_std.input,
            book.lookup("cursor-grok-4.6").unwrap().input
        );
        assert!(grok_fast.input > grok_std.input);
        assert_eq!(
            book.lookup("cursor-grok-4.6-high-fast").unwrap().output,
            book.lookup("cursor-grok-4.6-fast").unwrap().output
        );

        let million = TokenSlice {
            input: 1_000_000,
            ..TokenSlice::default()
        };
        let cost = book.cost_usd("composer-2.5-fast", million).unwrap();
        assert!((cost - 3.0).abs() < 1e-9);
        assert!(book.lookup("composer-1").is_none());
    }

    #[test]
    fn cursor_fast_does_not_inherit_xai_grok_standard() {
        let book = empty_book();
        let xai = book.lookup("xai/grok-4.6").unwrap();
        let cursor_fast = book.lookup("cursor-grok-4.6-xhigh-fast").unwrap();
        assert!(cursor_fast.input > xai.input);
        let grok45_fast = book.lookup("cursor-grok-4.5-medium-fast").unwrap();
        assert_eq!(grok45_fast.output, 1.8e-5);
    }

    #[test]
    fn import_accepts_litellm_raw_and_condensed_snapshot() {
        let raw = serde_json::json!({
            "sample_spec": {"input_cost_per_token": 0.0, "output_cost_per_token": 0.0},
            "test-import-model": {
                "input_cost_per_token": 1e-6,
                "output_cost_per_token": 2e-6,
                "cache_read_input_token_cost": 1e-7
            }
        });
        let file = parse_import(raw.to_string().as_bytes()).unwrap();
        let m = &file.models["test-import-model"];
        assert_eq!(m.input, 1e-6);
        assert_eq!(m.cache_read, Some(1e-7));
        assert!(file.updated_at.is_some());

        // 精简快照保留自己的 updated_at（即上游拉取时刻）
        let condensed = serde_json::json!({
            "updated_at": "2026-09-01T00:00:00Z",
            "models": {"x-model": {"input": 3e-6, "output": 4e-6}}
        });
        let file = parse_import(condensed.to_string().as_bytes()).unwrap();
        assert_eq!(file.updated_at.as_deref(), Some("2026-09-01T00:00:00Z"));
        assert_eq!(file.models["x-model"].output, 4e-6);

        assert!(parse_import(b"not json").is_err());
        assert!(parse_import(b"{\"foo\": 1}").is_err());
    }

    #[test]
    fn imported_cache_overrides_embedded_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let condensed =
            serde_json::json!({"models": {"claude-opus-5": {"input": 1.0, "output": 2.0}}});
        let n = import_and_store(dir.path(), condensed.to_string().as_bytes()).unwrap();
        assert_eq!(n, 1);
        let book = PriceBook::load(dir.path(), None).unwrap();
        assert!(book.cache_applied);
        assert_eq!(book.lookup("claude-opus-5").unwrap().input, 1.0);
    }

    #[test]
    fn cache_older_than_snapshot_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot_price = empty_book().lookup("claude-opus-5").unwrap().input;
        let stale = serde_json::json!({
            "updated_at": "2000-01-01T00:00:00Z",
            "models": {"claude-opus-5": {"input": 1.0, "output": 2.0}}
        });
        std::fs::write(dir.path().join("pricing.json"), stale.to_string()).unwrap();
        let book = PriceBook::load(dir.path(), None).unwrap();
        assert!(!book.cache_applied);
        assert_eq!(book.lookup("claude-opus-5").unwrap().input, snapshot_price);
        assert_eq!(book.updated_at, empty_book().updated_at);
        // 没有时间戳的缓存照旧生效
        let undated =
            serde_json::json!({"models": {"claude-opus-5": {"input": 1.0, "output": 2.0}}});
        std::fs::write(dir.path().join("pricing.json"), undated.to_string()).unwrap();
        assert!(PriceBook::load(dir.path(), None).unwrap().cache_applied);
    }
}
