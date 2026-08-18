use crate::error::CliError;
use crate::hooks::install_session_start_hooks;
use crate::output::Output;
use clap::{Args, Subcommand};
use serde_json::{Value, json};

#[derive(Args, Debug)]
pub struct SetupArgs {
    #[command(subcommand)]
    pub sub: SetupSub,
}

#[derive(Subcommand, Debug)]
pub enum SetupSub {
    /// Install the SessionStart ambient-context hook (Claude Code, Codex, OpenCode)
    Hooks,
}

pub fn run(_args: &SetupArgs) -> Result<Output, CliError> {
    let results = install_session_start_hooks("deepseek-axi", "deepseek-axi", 10);
    let targets: Vec<Value> = results
        .into_iter()
        .map(|(target, outcome)| match outcome {
            Ok(status) => json!({"target": target, "status": status}),
            Err(e) => json!({"target": target, "status": "error", "message": e.message}),
        })
        .collect();
    Ok(Output::Data(json!({
        "setup": targets,
        "help": [
            "Restart your agent session to pick these up",
            "Claude Code / Codex / OpenCode get the home view as ambient context at session start",
            "DeepSeek Harness gets a model-invoked skill instead — it already talks to DeepSeek, so only the worktree isolation and two-sided verification are worth surfacing",
        ],
    })))
}
