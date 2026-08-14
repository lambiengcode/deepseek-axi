use crate::error::CliError;
use crate::models::model_info;
use crate::output::Output;
use clap::Args;
use serde_json::json;

#[derive(Args, Debug)]
pub struct ModelsArgs {}

pub fn run(_args: &ModelsArgs) -> Result<Output, CliError> {
    let info = model_info();
    Ok(Output::Data(json!({
        "count": info.len(),
        "models": info,
        "help": [
            "Run `deepseek-axi ask \"<prompt>\" --model pro` to force a model",
            "Default model is flash unless overridden",
        ],
    })))
}
