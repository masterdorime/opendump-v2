//! Opt-in cloud classifier.
//!
//! Deliberately separate from `ai.rs`: that path speaks plain HTTP to
//! `127.0.0.1`, this one is TLS to whatever endpoint the user configured. It has
//! never been run against a live endpoint, which is why it is off by default,
//! shows its payload before sending, times out hard, and keeps the key in memory
//! only — nothing about it is trusted yet.

use std::time::Duration;

use serde_json::{json, Value};

use crate::ai::{parse_verdicts, ClassifyItem, Verdict};

const TIMEOUT: Duration = Duration::from_secs(45);

const PROMPT: &str = "You audit folders on a Windows drive. For each folder decide KEEP or REVIEW. \
Never say a folder is safe to delete: REVIEW is the strongest verdict you may give. \
Answer with JSON only, no prose: {\"results\":[{\"path\":\"the exact path given\",\"tier\":\"KEEP\" or \"REVIEW\",\"reason\":\"one sentence\"}]}";

pub struct CloudConfig {
    /// Full chat-completions URL, e.g. `https://host/v1/chat/completions`.
    pub endpoint: String,
    pub model: String,
    /// Held in memory only; never written to disk.
    pub key: String,
}

/// Exactly what would leave the machine, with the key redacted.
///
/// The UI shows this before the first call, so the disclosure is a thing the
/// user can read rather than a promise in a sentence.
pub fn preview(config: &CloudConfig, items: &[ClassifyItem]) -> String {
    let payload = json!({
        "model": config.model,
        "temperature": 0,
        "messages": [
            {"role": "system", "content": PROMPT},
            {"role": "user", "content": folders(items)},
        ],
    });
    serde_json::to_string_pretty(&payload).unwrap_or_default()
}

fn folders(items: &[ClassifyItem]) -> String {
    let rows: Vec<Value> = items
        .iter()
        .map(|item| {
            json!({
                "path": item.path,
                "size_bytes": item.size,
                "files": item.files,
                "matched_category": item.category,
            })
        })
        .collect();
    serde_json::to_string(&rows).unwrap_or_default()
}

/// Strip ```json fences, which models add even when told not to.
fn bare(text: &str) -> &str {
    let trimmed = text.trim();
    let without_open = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    without_open.trim_end_matches("```").trim()
}

pub fn classify(config: &CloudConfig, items: &[ClassifyItem]) -> Result<Vec<Verdict>, String> {
    if config.endpoint.trim().is_empty() {
        return Err("no endpoint configured".into());
    }
    let body = preview(config, items);

    let response = ureq::post(config.endpoint.trim())
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {}", config.key))
        .timeout(TIMEOUT)
        .send_string(&body)
        .map_err(|error| format!("cloud request failed: {error}"))?;

    let text = response
        .into_string()
        .map_err(|error| format!("cloud response could not be read: {error}"))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| format!("cloud response was not JSON: {error}"))?;

    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| "response had no choices[0].message.content".to_string())?;

    let allowed: Vec<String> = items.iter().map(|item| item.path.clone()).collect();
    // Same parser as the local path: invented paths are dropped and anything
    // that is not exactly KEEP becomes REVIEW.
    parse_verdicts(bare(content), &allowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CloudConfig {
        CloudConfig {
            endpoint: "https://example.invalid/v1/chat/completions".into(),
            model: "some-model".into(),
            key: "secret".into(),
        }
    }

    #[test]
    fn the_preview_contains_only_metadata_and_never_the_key() {
        let items = vec![ClassifyItem {
            path: r"C:\Users\someone\AppData\Local\SomeApp\Cache".into(),
            size: 6_300_000_000,
            files: 1_762_027,
            category: "Review".into(),
        }];
        let text = preview(&config(), &items);

        assert!(text.contains("SomeApp"));
        assert!(text.contains("6300000000"));
        assert!(!text.contains("secret"), "the key must never appear");
        assert!(!text.contains(".png"), "file names are not sent");
    }

    #[test]
    fn fenced_json_is_unwrapped() {
        assert_eq!(bare("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(bare("```\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(bare("  {\"a\":1}  "), "{\"a\":1}");
    }

    #[test]
    fn an_empty_endpoint_fails_before_any_request() {
        let mut cfg = config();
        cfg.endpoint = "   ".into();
        let error = classify(&cfg, &[]).unwrap_err();
        assert!(error.contains("no endpoint"));
    }
}
