//! DeepSeek API client — chat completions, FIM completion, and a streaming
//! SSE parser. Mirrors `api.js`: every HTTP error path routes through one
//! function so every caller gets the same `CliError` code mapping
//! (api-docs.deepseek.com/quick_start/error_codes).
//!
//! Known parity gap, disclosed not hidden (matches the Node build): the SSE
//! streaming path (`parse_sse`) is built and tested but not wired into any
//! command yet — every real call today uses the non-streaming path, exactly
//! like the Node build's `chatCompletion()` is never called with `onDelta`
//! from any consumer.
#![allow(dead_code)] // FIM (`completion`) and streaming (`parse_sse` et al.) — built, tested, intentionally not yet wired into a command; see module doc above.

use crate::error::CliError;
use serde_json::{Value, json};
use std::io::Read;
use std::time::Duration;

fn base_url() -> String {
    std::env::var("DEEPSEEK_BASE_URL").unwrap_or_else(|_| "https://api.deepseek.com".to_string())
}

const DEFAULT_TIMEOUT_MS: u64 = 300_000;

fn timeout() -> Duration {
    let ms = std::env::var("DEEPSEEK_AXI_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    Duration::from_millis(ms)
}

pub fn require_api_key() -> Result<String, CliError> {
    std::env::var("DEEPSEEK_API_KEY").map_err(|_| {
        CliError::validation(
            "DEEPSEEK_API_KEY is not set",
            vec![
                "Run `export DEEPSEEK_API_KEY=sk-...`".into(),
                "Get a key at https://platform.deepseek.com/api_keys".into(),
            ],
        )
    })
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// All error paths route through here (mirrors `api.js`'s `request()`).
fn request(path: &str, body: &Value) -> Result<(u16, String), CliError> {
    let api_key = require_api_key()?;
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout()))
        .http_status_as_error(false)
        .build();
    let agent: ureq::Agent = config.into();

    let url = format!("{}{}", base_url(), path);
    let sent = agent
        .post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send_json(body);

    let mut response = sent.map_err(|e| {
        let reason = if e.to_string().to_lowercase().contains("timeout") {
            format!("timed out after {}ms", timeout().as_millis())
        } else {
            e.to_string()
        };
        CliError::new(
            format!("Could not reach DeepSeek API: {reason}"),
            "NETWORK_ERROR",
            vec!["Retry, or raise the limit with DEEPSEEK_AXI_TIMEOUT_MS".into()],
        )
    })?;

    let status = response.status().as_u16();
    let mut body_text = String::new();
    let _ = response.body_mut().as_reader().read_to_string(&mut body_text);

    match status {
        401 => Err(CliError::new(
            "DeepSeek API rejected the key (401)",
            "AUTH_ERROR",
            vec!["Check DEEPSEEK_API_KEY at https://platform.deepseek.com/api_keys".into()],
        )),
        402 => Err(CliError::new(
            "DeepSeek account has insufficient balance (402)",
            "INSUFFICIENT_BALANCE",
            vec![
                "Top up at https://platform.deepseek.com".into(),
                "This will not resolve by retrying".into(),
            ],
        )),
        422 => Err(CliError::new(
            format!(
                "DeepSeek API rejected the request parameters (422): {}",
                truncate(&body_text, 300)
            ),
            "VALIDATION_ERROR",
            vec![],
        )),
        429 => Err(CliError::new(
            "Rate limited by DeepSeek API (429)",
            "RATE_LIMIT",
            vec!["Wait a moment and retry".into()],
        )),
        200..=299 => Ok((status, body_text)),
        _ => Err(CliError::new(
            format!("DeepSeek API error {status}: {}", truncate(&body_text, 300)),
            "API_ERROR",
            vec![],
        )),
    }
}

fn parse_json_body(body_text: &str) -> Result<Value, CliError> {
    serde_json::from_str(body_text).map_err(|e| {
        CliError::new(
            format!("DeepSeek API returned a non-JSON response body: {e}"),
            "API_ERROR",
            vec![],
        )
    })
}

/// POST /chat/completions (non-streaming). `payload` should already contain
/// `model`/`messages`/etc; `stream` is forced to `false` unless already set.
pub fn chat_completion(mut payload: Value) -> Result<Value, CliError> {
    if payload.get("stream").is_none() {
        payload["stream"] = json!(false);
    }
    let (_, body_text) = request("/chat/completions", &payload)?;
    parse_json_body(&body_text)
}

/// POST /beta/completions — Fill-In-the-Middle. deepseek-v4-pro only,
/// `prompt`+`suffix` (api-docs.deepseek.com/api/create-completion).
pub fn completion(mut payload: Value) -> Result<Value, CliError> {
    if payload.get("stream").is_none() {
        payload["stream"] = json!(false);
    }
    let (_, body_text) = request("/beta/completions", &payload)?;
    parse_json_body(&body_text)
}

// ---- Streaming SSE parser (built + tested, not wired into any command yet — see module doc) ----

#[derive(Default, Debug, Clone)]
pub struct ToolCallAccumulator {
    pub id: Option<String>,
    pub kind: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Default, Debug)]
pub struct StreamedMessage {
    pub content: String,
    pub reasoning_content: String,
    pub tool_calls: Vec<ToolCallAccumulator>,
    pub usage: Option<Value>,
}

fn merge_tool_call_deltas(acc: &mut Vec<Option<ToolCallAccumulator>>, deltas: &[Value]) {
    for delta in deltas {
        let index = delta.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        while acc.len() <= index {
            acc.push(None);
        }
        let entry = acc[index].get_or_insert_with(ToolCallAccumulator::default);
        if let Some(id) = delta.get("id").and_then(Value::as_str) {
            entry.id = Some(id.to_string());
        }
        if entry.kind.is_empty() {
            entry.kind = delta
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("function")
                .to_string();
        }
        if let Some(func) = delta.get("function") {
            if let Some(name) = func.get("name").and_then(Value::as_str) {
                entry.name.push_str(name);
            }
            if let Some(args) = func.get("arguments").and_then(Value::as_str) {
                entry.arguments.push_str(args);
            }
        }
    }
}

/// Parses a DeepSeek SSE stream (`data: {...}\n\n`, terminated by
/// `data: [DONE]`) into the same shape a non-streaming call returns.
/// `on_delta` is invoked once per chunk's delta object for progressive
/// feedback (unused by any current caller — see module doc).
pub fn parse_sse<R: Read>(reader: R, mut on_delta: impl FnMut(&Value)) -> StreamedMessage {
    use std::io::{BufRead, BufReader};
    let mut result = StreamedMessage::default();
    let mut tool_calls_acc: Vec<Option<ToolCallAccumulator>> = Vec::new();
    let buffered = BufReader::new(reader);

    for line in buffered.lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if !trimmed.starts_with("data:") {
            continue;
        }
        let data = trimmed.trim_start_matches("data:").trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let chunk: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue, // skip a malformed chunk rather than aborting the whole stream
        };

        let delta = chunk
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("delta"))
            .cloned()
            .unwrap_or(Value::Null);

        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            result.content.push_str(content);
        }
        if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
            result.reasoning_content.push_str(reasoning);
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            merge_tool_call_deltas(&mut tool_calls_acc, tool_calls);
        }
        if let Some(usage) = chunk.get("usage")
            && !usage.is_null()
        {
            result.usage = Some(usage.clone());
        }
        on_delta(&delta);
    }

    result.tool_calls = tool_calls_acc.into_iter().flatten().collect();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn parse_sse_accumulates_content_reasoning_and_tool_calls() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking...\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"read_\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"file\",\"arguments\":\"{\\\"path\\\":\\\"a.js\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":10}}\n\n",
            "data: [DONE]\n\n",
        );
        let mut delta_count = 0;
        let result = parse_sse(Cursor::new(sse.as_bytes()), |_| delta_count += 1);
        assert_eq!(result.content, "Hello");
        assert_eq!(result.reasoning_content, "thinking...");
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "read_file");
        assert_eq!(result.tool_calls[0].arguments, "{\"path\":\"a.js\"}");
        assert_eq!(
            result.usage.unwrap().get("prompt_tokens").and_then(Value::as_u64),
            Some(10)
        );
        assert!(delta_count > 0);
    }

    #[test]
    fn parse_sse_skips_malformed_chunks_without_aborting() {
        let sse = "data: not json at all\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
        let result = parse_sse(Cursor::new(sse.as_bytes()), |_| {});
        assert_eq!(result.content, "ok");
    }

    #[test]
    fn require_api_key_is_validation_error_when_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        let original = std::env::var("DEEPSEEK_API_KEY").ok();
        unsafe {
            std::env::remove_var("DEEPSEEK_API_KEY");
        }
        let err = require_api_key().unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
        if let Some(v) = original {
            unsafe {
                std::env::set_var("DEEPSEEK_API_KEY", v);
            }
        }
    }

    #[test]
    fn timeout_defaults_to_300_seconds_when_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        let original = std::env::var("DEEPSEEK_AXI_TIMEOUT_MS").ok();
        unsafe {
            std::env::remove_var("DEEPSEEK_AXI_TIMEOUT_MS");
        }
        assert_eq!(timeout(), Duration::from_secs(300));
        if let Some(v) = original {
            unsafe {
                std::env::set_var("DEEPSEEK_AXI_TIMEOUT_MS", v);
            }
        }
    }

    #[test]
    fn timeout_respects_deepseek_axi_timeout_ms_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let original = std::env::var("DEEPSEEK_AXI_TIMEOUT_MS").ok();
        unsafe {
            std::env::set_var("DEEPSEEK_AXI_TIMEOUT_MS", "45000");
        }
        assert_eq!(timeout(), Duration::from_millis(45_000));
        match original {
            Some(v) => unsafe { std::env::set_var("DEEPSEEK_AXI_TIMEOUT_MS", v) },
            None => unsafe { std::env::remove_var("DEEPSEEK_AXI_TIMEOUT_MS") },
        }
    }
}
