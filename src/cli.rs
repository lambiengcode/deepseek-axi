use crate::commands::{ask, cache, home, inspect, models, review, run, setup, test_fix, think, update};
use crate::error::CliError;
use crate::output::Output;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "deepseek-axi",
    version = env!("CARGO_PKG_VERSION"),
    about = "Delegate reasoning, long-context work, and autonomous fixes to DeepSeek V4"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// One-shot query. Defaults to deepseek-v4-flash.
    Ask(ask::AskArgs),
    /// Forces deepseek-v4-pro with thinking mode on.
    Think(think::ThinkArgs),
    /// Reads file(s) and returns structured findings.
    Review(review::ReviewArgs),
    /// Lists deepseek-v4-flash / deepseek-v4-pro.
    Models(models::ModelsArgs),
    /// Cache hit/miss stats.
    Cache(cache::CacheArgs),
    /// Ambient-context hook installation.
    Setup(setup::SetupArgs),
    /// Read-only context discovery.
    Inspect(inspect::InspectArgs),
    /// Full plan->implement->test->verify loop against DeepSeek V4.
    Run(run::RunArgs),
    /// Focused test->diagnose->fix loop.
    Test(test_fix::TestArgs),
    /// Self-update via GitHub Releases.
    Update(update::UpdateArgs),
}

pub fn dispatch(command: &Option<Command>) -> Result<Output, CliError> {
    match command {
        None => home::run(),
        Some(Command::Ask(a)) => ask::run(a),
        Some(Command::Think(a)) => think::run(a),
        Some(Command::Review(a)) => review::run(a),
        Some(Command::Models(a)) => models::run(a),
        Some(Command::Cache(a)) => cache::run(a),
        Some(Command::Setup(a)) => setup::run(a),
        Some(Command::Inspect(a)) => inspect::run(a),
        Some(Command::Run(a)) => run::run(a),
        Some(Command::Test(a)) => test_fix::run(a),
        Some(Command::Update(a)) => update::run(a),
    }
}
