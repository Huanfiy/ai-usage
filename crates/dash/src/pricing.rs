use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const SNAPSHOT: &[u8] = include_bytes!("../pricing/litellm-snapshot.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceBookFile {
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub models: HashMap<String, ModelPrice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
#[allow(dead_code)]
pub struct PriceBook {
    pub updated_at: Option<String>,
    index: HashMap<String, ModelPrice>,
}

impl PriceBook {
    pub fn load(data_dir: &Path, override_path: Option<&Path>) -> Result<Self> {
        let snapshot: PriceBookFile =
            serde_json::from_slice(SNAPSHOT).context("嵌入报价快照损坏")?;
        let mut merged = snapshot.models;
        let mut updated_at = snapshot.updated_at;
        let cache_path = data_dir.join("pricing.json");
        if cache_path.is_file() {
            if let Ok(file) = std::fs::read(&cache_path) {
                if let Ok(cache) = serde_json::from_slice::<PriceBookFile>(&file) {
                    for (k, v) in cache.models {
                        merged.insert(k, v);
                    }
                    if cache.updated_at.is_some() {
                        updated_at = cache.updated_at;
                    }
                }
            }
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
            index: build_index(merged),
        })
    }

    pub fn lookup(&self, model: &str) -> Option<&ModelPrice> {
        let n = normalize_model(model);
        if let Some(p) = self.index.get(&n) {
            return Some(p);
        }
        if let Some(stripped) = n.strip_suffix("-build") {
            if let Some(p) = self.index.get(stripped) {
                return Some(p);
            }
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

pub fn fetch_and_store(data_dir: &Path) -> Result<usize> {
    let resp = ureq::get(LITELLM_URL)
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let raw: serde_json::Value = resp.into_json()?;
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
    std::fs::create_dir_all(data_dir)?;
    let file = PriceBookFile {
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
        models,
    };
    let n = file.models.len();
    std::fs::write(
        data_dir.join("pricing.json"),
        serde_json::to_vec_pretty(&file)?,
    )?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_canonical_models() {
        let book = PriceBook::load(Path::new("/tmp/does-not-exist-ai-usage"), None).unwrap();
        assert!(book.lookup("claude-opus-5").is_some());
        assert!(book.lookup("gpt-5.4").is_some());
        assert!(book.lookup("grok-4.6-build").is_some());
        assert!(book.lookup("xai/grok-4.6").is_some());
    }
}
