use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

use crate::util::UsageEntry;

const SOURCE: &str = ai_usage_protocol::SOURCE_CURSOR;

const COL_DATE: &str = "Date";
const COL_MODEL: &str = "Model";
const COL_INPUT: &str = "Input (w/o Cache Write)";
const COL_CACHE_WRITE: &str = "Input (w/ Cache Write)";
const COL_CACHE_READ: &str = "Cache Read";
const COL_OUTPUT: &str = "Output Tokens";

#[derive(Debug)]
pub struct CsvError;

/// Full CSV. Do not time-window-truncate: agent `state.prune` would drop the
/// omitted buckets and re-upload them as new on the next successful parse.
pub fn parse_export_csv(text: &str) -> Result<Vec<UsageEntry>, CsvError> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(text.as_bytes());
    let headers = reader.headers().map_err(|_| CsvError)?.clone();
    let idx = |name: &str| {
        headers
            .iter()
            .position(|h| header_name(h) == name)
            .unwrap_or(usize::MAX)
    };
    let date_i = idx(COL_DATE);
    let model_i = idx(COL_MODEL);
    if date_i == usize::MAX || model_i == usize::MAX {
        return Err(CsvError);
    }
    let input_i = idx(COL_INPUT);
    let cache_write_i = idx(COL_CACHE_WRITE);
    let cache_read_i = idx(COL_CACHE_READ);
    let output_i = idx(COL_OUTPUT);

    let mut entries = Vec::new();
    for rec in reader.records() {
        let rec = rec.map_err(|_| CsvError)?;
        if rec.iter().all(|f| f.trim().is_empty()) {
            continue;
        }
        let Some(ts) = rec.get(date_i).and_then(parse_csv_date) else {
            continue;
        };
        let model = rec.get(model_i).map(str::trim).unwrap_or("");
        if model.is_empty() {
            continue;
        }
        let input = cell(&rec, input_i);
        let cache_write = cell(&rec, cache_write_i);
        let cache_read = cell(&rec, cache_read_i);
        let output = cell(&rec, output_i);
        if input + cache_write + cache_read + output == 0 {
            continue;
        }
        entries.push(UsageEntry {
            source: SOURCE.into(),
            model: model.to_string(),
            project: "unknown".into(),
            timestamp: ts,
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_write,
            reasoning_output_tokens: 0,
        });
    }
    Ok(entries)
}

fn header_name(s: &str) -> String {
    s.trim().trim_start_matches('\u{feff}').to_string()
}

fn cell(rec: &csv::StringRecord, idx: usize) -> i64 {
    rec.get(idx).map(parse_count).unwrap_or(0)
}

fn parse_count(raw: &str) -> i64 {
    let t = raw.replace(',', "").trim().to_string();
    if t.is_empty() {
        return 0;
    }
    t.parse::<f64>()
        .ok()
        .map(|f| f.round() as i64)
        .unwrap_or(0)
        .max(0)
}

fn parse_csv_date(raw: &str) -> Option<DateTime<Utc>> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(d) = NaiveDate::parse_from_str(t, "%Y-%m-%d") {
        return naive_local_to_utc(d.and_hms_opt(0, 0, 0)?);
    }
    const FMTS: &[&str] = &[
        "%B %d, %Y, %I:%M %p",
        "%B %d, %Y, %I:%M:%S %p",
        "%b %d, %Y, %I:%M %p",
        "%b %d, %Y, %I:%M:%S %p",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M:%SZ",
    ];
    for fmt in FMTS {
        if let Ok(naive) = NaiveDateTime::parse_from_str(t, fmt) {
            return naive_local_to_utc(naive);
        }
    }
    None
}

fn naive_local_to_utc(naive: NaiveDateTime) -> Option<DateTime<Utc>> {
    Local
        .from_local_datetime(&naive)
        .single()
        .or_else(|| Local.from_local_datetime(&naive).earliest())
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> String {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/cursor/export.csv");
        fs::read_to_string(path).expect("fixtures/cursor/export.csv")
    }

    #[test]
    fn splits_cache_fields_skips_zero_and_quoted_model() {
        let entries = parse_export_csv(&fixture()).unwrap();
        assert_eq!(entries.len(), 2, "zero-token row must be skipped");

        let quoted = entries
            .iter()
            .find(|e| e.model == "composer, experimental")
            .unwrap();
        assert_eq!(quoted.input_tokens, 15);
        assert_eq!(quoted.cache_creation_input_tokens, 100);
        assert_eq!(quoted.cache_read_input_tokens, 250);
        assert_eq!(quoted.output_tokens, 33);
        assert_eq!(quoted.project, "unknown");
        assert_eq!(quoted.source, SOURCE);

        let iso = entries.iter().find(|e| e.model == "composer-1").unwrap();
        assert_eq!(iso.input_tokens, 80);
        assert_eq!(iso.cache_creation_input_tokens, 0);
        assert_eq!(iso.cache_read_input_tokens, 0);
        assert_eq!(iso.output_tokens, 20);
        assert_eq!(iso.timestamp.to_rfc3339(), "2026-01-15T10:45:00+00:00");
    }

    #[test]
    fn comma_date_is_parseable() {
        assert!(parse_csv_date("January 15, 2026, 10:17 AM").is_some());
    }
}
