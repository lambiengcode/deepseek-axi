use crate::context_engine::build_context;
use crate::error::CliError;
use crate::output::Output;
use clap::Args;
use serde_json::json;

#[derive(Args, Debug)]
pub struct InspectArgs {
    #[arg(long)]
    pub task: String,
    /// Force-include specific files regardless of discovery (repeatable)
    #[arg(long)]
    pub files: Vec<String>,
    /// Token budget for the assembled context (default 8000)
    #[arg(long, default_value_t = 8000)]
    pub budget: usize,
    /// Cap how many related files discovery can fan out to (default 20)
    #[arg(long = "max-files", default_value_t = 20)]
    pub max_files: usize,
    /// Also emit the full assembled prompt text
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: &InspectArgs) -> Result<Output, CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::new(format!("Could not read current directory: {e}"), "API_ERROR", vec![]))?;
    let (built, prompt) = build_context(&args.task, &cwd, &args.files, args.budget, args.max_files);

    let mut data = json!({
        "task": args.task,
        "branch": built.branch,
        "touchedFiles": built.touched_files,
        "relatedFiles": built.related_files,
        "testFiles": built.test_files,
        "tokensEstimate": built.tokens_estimate,
        "included": built.included,
        "dropped": built.dropped,
        "help": ["Run `deepseek-axi inspect ... --json` to see the full assembled prompt text"],
    });
    if args.json {
        data["prompt"] = json!(prompt);
    }

    Ok(Output::Data(data))
}
