use crate::error::CliError;
use crate::output::Output;
use crate::usage_store::read_usage;
use serde_json::json;

pub fn run() -> Result<Output, CliError> {
    let api_key_configured = std::env::var("DEEPSEEK_API_KEY").is_ok();
    let usage = read_usage();
    let bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "deepseek-axi".into());

    let help: Vec<&str> = if api_key_configured {
        vec![
            "Run `deepseek-axi ask \"<prompt>\"` for a quick answer",
            "Run `deepseek-axi think \"<prompt>\"` for hard reasoning (deepseek-v4-pro)",
            "Run `deepseek-axi review <path...>` to review files with DeepSeek",
            "Run `deepseek-axi run --task \"<text>\"` to delegate a real code change",
            "Run `deepseek-axi cache stats` to see cache hit rate",
        ]
    } else {
        vec![
            "Set DEEPSEEK_API_KEY to get started: export DEEPSEEK_API_KEY=sk-...",
            "Get a key at https://platform.deepseek.com/api_keys",
        ]
    };

    Ok(Output::Data(json!({
        "bin": bin,
        "description": "Delegate reasoning, long-context work, and autonomous fixes to DeepSeek V4",
        "apiKey": if api_key_configured { "configured" } else { "missing" },
        "defaultModel": "deepseek-v4-flash",
        "calls": usage.calls,
        "help": help,
    })))
}
