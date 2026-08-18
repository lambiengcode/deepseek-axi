use crate::api::chat_completion;
use crate::error::CliError;
use crate::files::read_files_for_prompt;
use crate::models::resolve_model;
use crate::output::Output;
use crate::sanitize::sanitize_for_terminal;
use crate::time_util::now_iso;
use crate::usage_store::record_usage;
use clap::Args;
use serde_json::{Value, json};

const REVIEW_INSTRUCTIONS: &str = "You are reviewing source files for concrete, verifiable issues (bugs, edge cases, security problems). Respond ONLY with JSON matching exactly this shape: {\"findings\": [{\"file\": string, \"line\": number|null, \"severity\": \"high\"|\"medium\"|\"low\", \"summary\": string}]}. Return {\"findings\": []} if nothing to report. No prose outside the JSON.";

#[derive(Args, Debug)]
pub struct ReviewArgs {
    /// File path(s) to review
    #[arg(required = true)]
    pub paths: Vec<String>,
    #[arg(long)]
    pub model: Option<String>,
    /// Keep only findings whose file or summary matches (substring, case-insensitive)
    #[arg(long)]
    pub query: Option<String>,
}

pub fn run(args: &ReviewArgs) -> Result<Output, CliError> {
    let model = resolve_model(args.model.as_deref())?;
    let files_block = read_files_for_prompt(&args.paths)?;

    let payload = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": REVIEW_INSTRUCTIONS},
            {"role": "user", "content": files_block},
        ],
        "thinking": {"type": "disabled"},
        "response_format": {"type": "json_object"},
        // Findings are a compact JSON array; 4096 is far above any real result.
        "max_tokens": 4096,
    });

    let response = chat_completion(payload)?;
    record_usage(&model, response.get("usage"), now_iso(), None);

    let raw = response
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("{}");

    let parsed: Value = serde_json::from_str(raw)
        .map_err(|_| CliError::new("DeepSeek did not return valid JSON for review", "API_ERROR", vec![]))?;

    let mut findings: Vec<Value> = parsed
        .get("findings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut f| {
            if let Some(file) = f.get("file").and_then(Value::as_str).map(sanitize_for_terminal) {
                f["file"] = json!(file);
            }
            if let Some(summary) = f.get("summary").and_then(Value::as_str).map(sanitize_for_terminal) {
                f["summary"] = json!(summary);
            }
            f
        })
        .collect();

    if let Some(query) = &args.query {
        let needle = query.to_lowercase();
        findings.retain(|f| {
            let file = f.get("file").and_then(Value::as_str).unwrap_or("");
            let summary = f.get("summary").and_then(Value::as_str).unwrap_or("");
            format!("{file} {summary}").to_lowercase().contains(&needle)
        });
    }

    Ok(Output::Data(json!({
        "filesReviewed": args.paths.len(),
        "count": findings.len(),
        "findings": findings,
    })))
}
