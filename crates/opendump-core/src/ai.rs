//! Local AI: ask an Ollama model about folders the rules do not understand.
//!
//! Three rules, the same ones v1 enforces:
//!
//! 1. The ceiling is `REVIEW`. Anything stronger the model emits is coerced —
//!    a model cannot authorise deletion.
//! 2. Paths the model was never given are discarded, so an invented or injected
//!    path cannot enter the list.
//! 3. Missing confidence is neutral (0.5), and it is clamped to 0..1.
//!
//! Deliberately localhost-only and dependency-free: this is a plain HTTP/1.1
//! client for `127.0.0.1`, not a general transport. A cloud provider would need
//! TLS and is out of scope until someone actually tests one.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::{json, Value};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(600);

const SYSTEM_PROMPT: &str = r#"You help a Windows user decide which folders are safe to remove to free disk space.

For each folder you receive: its path, size, file count, last-modified date and the rule category that matched it.

Answer with a tier and a short factual reason:
- "REVIEW" — this might be removable, but a human must confirm. Use this when you are unsure.
- "KEEP" — this is user data, or you cannot establish what it is.

You must never assert that something is safe to delete. You cannot authorise deletion; the maximum you can say is REVIEW.

Reasoning guidance:
- Build caches, package manager caches, shader caches, crash dumps, temporary files and compiled artefacts regenerate automatically.
- Application data that contains user-created content (project files, saves, documents, media) must be KEEP.
- Emulator images, VM disks, model weights and downloaded toolchains are re-downloadable but expensive; that is REVIEW, not removal.
- If the folder name suggests a person, a client, a document or a project, prefer KEEP.
- If you do not recognise the application, say so and use low confidence.

Confidence is 0..1 and must reflect how certain you are that your *reason* is accurate, not how strong your opinion is.

Give one entry per input path, using the exact path string you were given."#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiTier {
    Review,
    Keep,
}

impl AiTier {
    pub fn as_str(self) -> &'static str {
        match self {
            AiTier::Review => "REVIEW",
            AiTier::Keep => "KEEP",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub path: String,
    pub tier: AiTier,
    pub reason: String,
    pub confidence: f32,
}

#[derive(Debug, Clone)]
pub struct ClassifyItem {
    pub path: String,
    pub size: u64,
    pub files: u64,
    pub category: String,
}

/// Parse a model response into verdicts, enforcing the three rules above.
pub fn parse_verdicts(raw: &str, allowed: &[String]) -> Result<Vec<Verdict>, String> {
    let parsed: Value =
        serde_json::from_str(raw).map_err(|error| format!("malformed JSON: {error}"))?;

    let results = parsed
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| "malformed output: expected an object with a \"results\" array".to_string())?;

    let canonical: HashMap<String, &String> = allowed
        .iter()
        .map(|path| (path.to_lowercase(), path))
        .collect();

    let mut out = Vec::new();
    for item in results {
        let Some(path) = item.get("path").and_then(Value::as_str) else {
            continue;
        };
        // Not in the batch: invented or injected. Drop it.
        let Some(original) = canonical.get(&path.to_lowercase()) else {
            continue;
        };

        // Anything that is not exactly KEEP becomes REVIEW.
        let tier = if item.get("tier").and_then(Value::as_str) == Some("KEEP") {
            AiTier::Keep
        } else {
            AiTier::Review
        };

        let mut confidence = item
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.5);
        if !confidence.is_finite() {
            confidence = 0.5;
        }

        let reason = item
            .get("reason")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("(no reason given)");

        out.push(Verdict {
            path: (*original).clone(),
            tier,
            reason: reason.to_string(),
            confidence: confidence.clamp(0.0, 1.0) as f32,
        });
    }

    Ok(out)
}

pub fn reachable(endpoint: &str) -> bool {
    request(endpoint, "GET", "/api/version", None).is_ok()
}

/// Model names installed in the local Ollama.
pub fn models(endpoint: &str) -> Vec<String> {
    let Ok(body) = request(endpoint, "GET", "/api/tags", None) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&body) else {
        return Vec::new();
    };

    parsed
        .get("models")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|model| model.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Ask the model about a batch. Blocking: call it off the UI thread.
pub fn classify(endpoint: &str, model: &str, items: &[ClassifyItem]) -> Result<Vec<Verdict>, String> {
    if items.is_empty() {
        return Ok(Vec::new());
    }

    let described: Vec<Value> = items
        .iter()
        .map(|item| {
            json!({
                "path": item.path,
                "sizeGB": (item.size as f64 / 1024f64.powi(3) * 100.0).round() / 100.0,
                "files": item.files,
                "lastModified": "unknown",
                "matchedCategory": item.category,
            })
        })
        .collect();

    let user = format!(
        "Classify these {} folders.\n\n{}",
        items.len(),
        serde_json::to_string_pretty(&described).unwrap_or_default()
    );

    let schema = json!({
        "type": "object",
        "properties": {
            "results": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "tier": { "type": "string", "enum": ["REVIEW", "KEEP"] },
                        "reason": { "type": "string" },
                        "confidence": { "type": "number" }
                    },
                    "required": ["path", "tier", "reason", "confidence"]
                }
            }
        },
        "required": ["results"]
    });

    let body = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": user }
        ],
        "stream": false,
        // Qwen 3.5 thinks by default; for labelling that is hundreds of wasted
        // tokens per folder and buys nothing.
        "think": false,
        "format": schema,
        "options": { "temperature": 0, "num_ctx": 4096 },
        // Unload after the batch: the runner costs ~1.3 GB of host RAM.
        "keep_alive": "0"
    });

    let raw = request(endpoint, "POST", "/api/chat", Some(&body.to_string()))?;
    let parsed: Value =
        serde_json::from_str(&raw).map_err(|error| format!("malformed response: {error}"))?;
    let content = parsed
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .ok_or_else(|| "malformed output: Ollama returned no message content".to_string())?;

    let allowed: Vec<String> = items.iter().map(|item| item.path.clone()).collect();
    parse_verdicts(content, &allowed)
}

fn request(
    endpoint: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<String, String> {
    let (host, port) = split_endpoint(endpoint)?;
    let address = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|error| format!("cannot resolve {endpoint}: {error}"))?
        .next()
        .ok_or_else(|| format!("cannot resolve {endpoint}"))?;

    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
        .map_err(|error| format!("cannot reach {endpoint}: {error}"))?;
    stream
        .set_read_timeout(Some(INFERENCE_TIMEOUT))
        .map_err(|error| error.to_string())?;

    let payload = body.unwrap_or("");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {endpoint}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );

    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(payload.as_bytes()))
        .map_err(|error| format!("write failed: {error}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|error| format!("read failed: {error}"))?;

    decode_http(&raw)
}

fn split_endpoint(endpoint: &str) -> Result<(String, u16), String> {
    let (host, port) = endpoint
        .rsplit_once(':')
        .ok_or_else(|| format!("endpoint must be host:port, got {endpoint}"))?;
    let port: u16 = port
        .parse()
        .map_err(|_| format!("bad port in {endpoint}"))?;
    Ok((host.to_string(), port))
}

fn decode_http(raw: &[u8]) -> Result<String, String> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "malformed HTTP response".to_string())?;

    let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    let body = &raw[split + 4..];

    if !head.starts_with("http/1.1 200") && !head.starts_with("http/1.0 200") {
        let status = head.lines().next().unwrap_or("no status");
        return Err(format!("Ollama returned {status}"));
    }

    let bytes = if head.contains("transfer-encoding: chunked") {
        decode_chunked(body)?
    } else {
        body.to_vec()
    };

    String::from_utf8(bytes).map_err(|error| format!("response was not UTF-8: {error}"))
}

fn decode_chunked(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();

    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| "truncated chunk header".to_string())?;

        let header = String::from_utf8_lossy(&body[..line_end]);
        let size_hex = header.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| format!("bad chunk size {size_hex:?}"))?;

        body = &body[line_end + 2..];
        if size == 0 {
            break;
        }
        if body.len() < size {
            return Err("truncated chunk body".to_string());
        }

        out.extend_from_slice(&body[..size]);
        body = &body[size..];
        if body.starts_with(b"\r\n") {
            body = &body[2..];
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed() -> Vec<String> {
        vec!["C:\\A".to_string(), "C:\\B".to_string()]
    }

    #[test]
    fn anything_stronger_than_review_is_coerced() {
        let raw = r#"{"results":[
            {"path":"C:\\A","tier":"SAFE_PURGE","reason":"cache","confidence":0.9},
            {"path":"C:\\B","tier":"DELETE","reason":"old","confidence":0.4}
        ]}"#;

        let verdicts = parse_verdicts(raw, &allowed()).expect("parse");
        assert_eq!(verdicts.len(), 2);
        assert!(verdicts.iter().all(|verdict| verdict.tier == AiTier::Review));
    }

    #[test]
    fn paths_outside_the_batch_are_dropped() {
        let raw = r#"{"results":[
            {"path":"C:\\A","tier":"KEEP","reason":"save file","confidence":1},
            {"path":"C:\\Windows\\System32","tier":"REVIEW","reason":"invented","confidence":1}
        ]}"#;

        let verdicts = parse_verdicts(raw, &allowed()).expect("parse");
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].path, "C:\\A");
    }

    #[test]
    fn matching_is_case_insensitive_but_the_batch_spelling_is_kept() {
        let raw = r#"{"results":[{"path":"c:\\a","tier":"KEEP","reason":"x","confidence":1}]}"#;
        let verdicts = parse_verdicts(raw, &allowed()).expect("parse");
        assert_eq!(verdicts[0].path, "C:\\A");
    }

    #[test]
    fn confidence_is_clamped_and_defaulted() {
        let raw = r#"{"results":[
            {"path":"C:\\A","tier":"KEEP","reason":"x","confidence":7},
            {"path":"C:\\B","tier":"KEEP","reason":"x"}
        ]}"#;

        let verdicts = parse_verdicts(raw, &allowed()).expect("parse");
        assert_eq!(verdicts[0].confidence, 1.0);
        assert_eq!(verdicts[1].confidence, 0.5);
    }

    #[test]
    fn a_missing_reason_gets_a_visible_placeholder() {
        let raw = r#"{"results":[{"path":"C:\\A","tier":"KEEP","reason":"  ","confidence":1}]}"#;
        let verdicts = parse_verdicts(raw, &allowed()).expect("parse");
        assert_eq!(verdicts[0].reason, "(no reason given)");
    }

    #[test]
    fn malformed_output_is_an_error_not_an_empty_success() {
        assert!(parse_verdicts("not json", &allowed()).is_err());
        assert!(parse_verdicts(r#"{"nope":[]}"#, &allowed()).is_err());
    }

    #[test]
    fn chunked_responses_are_decoded() {
        let body = b"4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        let decoded = decode_chunked(body).expect("decode");
        assert_eq!(String::from_utf8(decoded).unwrap(), "{\"a\":1}");
    }

    #[test]
    fn a_full_http_response_is_unwrapped() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"models\":[]}";
        assert_eq!(decode_http(raw).unwrap(), "{\"models\":[]}");

        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n";
        assert_eq!(decode_http(chunked).unwrap(), "{}");

        let failed = b"HTTP/1.1 500 Internal Server Error\r\n\r\n";
        assert!(decode_http(failed).is_err());
    }
}
