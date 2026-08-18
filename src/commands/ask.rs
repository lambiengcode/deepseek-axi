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

#[derive(Args, Debug)]
pub struct AskArgs {
    /// The prompt to ask
    pub prompt: String,
    /// Override the default model (default: flash)
    #[arg(long)]
    pub model: Option<String>,
    /// Inline a file's contents into the prompt (repeatable)
    #[arg(long)]
    pub files: Vec<String>,
    /// Ask DeepSeek to return a JSON object
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: &AskArgs) -> Result<Output, CliError> {
    let model = resolve_model(args.model.as_deref())?;
    let context_block = if args.files.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", read_files_for_prompt(&args.files)?)
    };

    let mut payload = json!({
        "model": model,
        "messages": [{"role": "user", "content": format!("{context_block}{}", args.prompt)}],
        // Thinking mode defaults to enabled server-side even for plain chat
        // completions; ask() never surfaces reasoning_content, so leaving it
        // on would silently burn reasoning tokens for nothing.
        "thinking": {"type": "disabled"},
        // Runaway guard: output is the priciest token class and nothing
        // else bounds a single response.
        "max_tokens": 8192,
    });
    if args.json {
        payload["response_format"] = json!({"type": "json_object"});
    }

    let response = chat_completion(payload)?;
    record_usage(&model, response.get("usage"), now_iso(), None);

    let content = extract_content(&response);
    Ok(Output::Text(sanitize_for_terminal(&content)))
}

pub fn extract_content(response: &Value) -> String {
    response
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}
