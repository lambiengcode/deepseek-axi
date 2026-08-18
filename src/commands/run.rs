use crate::error::CliError;
use crate::handoff::render_json;
use crate::models::resolve_model;
use crate::output::Output;
use crate::worker::worker_loop::{LoopOptions, run_worker_loop};
use clap::Args;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Args, Debug)]
pub struct RunArgs {
    #[arg(long)]
    pub task: String,
    /// Force-include specific files as context (repeatable)
    #[arg(long)]
    pub files: Vec<String>,
    /// Model to use (default: pro — this is hard work)
    #[arg(long)]
    pub model: Option<String>,
    /// Cap how many related files context discovery can fan out to (default 20)
    #[arg(long = "max-files", default_value_t = 20)]
    pub max_files: usize,
    /// Stop after N implement/test cycles (default: 6)
    #[arg(long = "max-iterations", default_value_t = 6)]
    pub max_iterations: u32,
    /// Stop once estimated cost exceeds this (soft, internal estimate only)
    #[arg(long = "max-cost")]
    pub max_cost: Option<f64>,
    /// Stop after this many seconds (default: 600)
    #[arg(long, default_value_t = 600)]
    pub timeout: u64,
    /// Plan only — no writes, no tests, no git changes
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    /// Edit the current working tree directly (opt-out of isolation)
    #[arg(long = "no-worktree")]
    pub no_worktree: bool,
    /// Only check the target passes; skip the pre-fix baseline run
    #[arg(long = "skip-regression-check")]
    pub skip_regression_check: bool,
    /// Run the base-commit verification concurrently with the model's turns.
    /// Faster, but two test suites can overlap — unsafe for suites that bind
    /// a fixed port or share a test database.
    #[arg(long = "parallel-verify")]
    pub parallel_verify: bool,
    /// Emit the full agent-to-agent handoff as real JSON (not TOON)
    #[arg(long)]
    pub json: bool,
    /// Emit just the prose summary
    #[arg(long)]
    pub summary: bool,
}

fn generate_task_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:08x}", (nanos as u64) ^ (std::process::id() as u64))[..8].to_string()
}

pub fn run(args: &RunArgs) -> Result<Output, CliError> {
    // run's default is pro (this is hard work), unlike ask's flash default —
    // resolve_model only handles the flash-default case, so branch explicitly.
    let model = match args.model.as_deref() {
        Some(m) => resolve_model(Some(m))?,
        None => crate::models::MODEL_PRO.to_string(),
    };
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::new(format!("Could not read current directory: {e}"), "API_ERROR", vec![]))?;
    let task_id = generate_task_id();

    let mut opts = LoopOptions::real(task_id, args.task.clone(), cwd);
    opts.files = args.files.clone();
    opts.model = model;
    opts.max_files = args.max_files;
    opts.max_iterations = args.max_iterations;
    opts.max_cost_usd = args.max_cost;
    opts.timeout = Duration::from_secs(args.timeout);
    opts.dry_run = args.dry_run;
    opts.use_worktree = !args.no_worktree;
    opts.skip_regression_check = args.skip_regression_check;
    opts.parallel_verify = args.parallel_verify;

    let handoff = run_worker_loop(opts)?;

    if args.json {
        return Ok(Output::Text(render_json(&handoff)));
    }
    if args.summary {
        return Ok(Output::Text(handoff.summary.clone()));
    }
    Ok(Output::Data(crate::handoff::render_trimmed(&handoff)))
}
