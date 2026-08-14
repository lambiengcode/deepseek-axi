use crate::api::chat_completion;
use crate::error::CliError;
use crate::files::read_files_for_prompt;
use crate::models::MODEL_PRO;
use crate::output::Output;
use crate::sanitize::sanitize_for_terminal;
use crate::time_util::now_iso;
use crate::usage_store::record_usage;
use clap::Args;
use serde_json::{Value, json};

#[derive(Args, Debug)]
pub struct ThinkArgs {
    pub prompt: String,
    /// Reasoning intensity: low|high|max (default: high)
    #[arg(long, default_value = "high")]
    pub effort: String,
    /// Print the reasoning trace before the answer
    #[arg(long)]
    pub full: bool,
    /// Inline a file's contents into the prompt (repeatable)
    #[arg(long)]
    pub files: Vec<String>,
}

pub fn run(args: &ThinkArgs) -> Result<Output, CliError> {
    if !["low", "high", "max"].contains(&args.effort.as_str()) {
        return Err(CliError::validation(
            format!("Invalid --effort: {}", args.effort),
            vec!["Use low, high, or max".into()],
        ));
    }

    let context_block = if args.files.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", read_files_for_prompt(&args.files)?)
    };

    let payload = json!({
        "model": MODEL_PRO,
        "messages": [{"role": "user", "content": format!("{context_block}{}", args.prompt)}],
        "thinking": {"type": "enabled"},
        "reasoning_effort": args.effort,
    });

    let response = chat_completion(payload)?;
    record_usage(MODEL_PRO, response.get("usage"), now_iso(), None);

    let message = response
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .cloned()
        .unwrap_or(Value::Null);
    let content = message.get("content").and_then(Value::as_str).unwrap_or("");
    let reasoning = message.get("reasoning_content").and_then(Value::as_str).unwrap_or("");
    let reasoning_tokens = response
        .get("usage")
        .and_then(|u| u.get("completion_tokens_details"))
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    if !reasoning.is_empty() && !args.full {
        eprintln!("reasoning_tokens: {reasoning_tokens} (use --full to show reasoning_content)");
    }

    let answer = sanitize_for_terminal(content);
    if args.full && !reasoning.is_empty() {
        let reasoning_clean = sanitize_for_terminal(reasoning);
        Ok(Output::Text(format!(
            "## Reasoning\n\n{reasoning_clean}\n\n## Answer\n\n{answer}"
        )))
    } else {
        Ok(Output::Text(answer))
    }
}
