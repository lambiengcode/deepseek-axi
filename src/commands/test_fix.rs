use crate::error::CliError;
use crate::handoff::render_json;
use crate::models::{MODEL_PRO, resolve_model};
use crate::output::Output;
use crate::worker::worker_loop::{LoopOptions, run_worker_loop};
use clap::Args;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Args, Debug)]
pub struct TestArgs {
    /// Required — this version only supports the fix loop
    #[arg(long)]
    pub fix: bool,
    /// Stop after N fix/test cycles (default: 4)
    #[arg(long = "max-iterations", default_value_t = 4)]
    pub max_iterations: u32,
    /// Model to use (default: pro). Repairing a failing test is often
    /// mechanical, and flash is ~3x cheaper on both prompt and output.
    #[arg(long)]
    pub model: Option<String>,
    /// Override the detected test command
    #[arg(long = "test-cmd")]
    pub test_cmd: Option<String>,
    /// Only check tests now pass; skip the pre-fix baseline run
    #[arg(long = "skip-regression-check")]
    pub skip_regression_check: bool,
    /// Run the base-commit verification concurrently with the model's turns.
    /// Faster, but two test suites can overlap — unsafe for suites that bind
    /// a fixed port or share a test database.
    #[arg(long = "parallel-verify")]
    pub parallel_verify: bool,
    /// Stop after this many seconds (default: 600)
    #[arg(long, default_value_t = 600)]
    pub timeout: u64,
    /// Edit the current working tree directly (opt-out of isolation)
    #[arg(long = "no-worktree")]
    pub no_worktree: bool,
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

pub fn run(args: &TestArgs) -> Result<Output, CliError> {
    if !args.fix {
        return Err(CliError::validation(
            "test requires --fix",
            vec![
                "Run `deepseek-axi test --fix`".into(),
                "There is no read-only `test` mode in this version".into(),
            ],
        ));
    }

    let cwd = std::env::current_dir()
        .map_err(|e| CliError::new(format!("Could not read current directory: {e}"), "API_ERROR", vec![]))?;
    let task_id = generate_task_id();

    let mut opts = LoopOptions::real(
        task_id,
        "Make the failing tests pass without breaking any currently-passing tests.".to_string(),
        cwd,
    );
    opts.model = match args.model.as_deref() {
        Some(m) => resolve_model(Some(m))?,
        None => MODEL_PRO.to_string(),
    };
    opts.max_iterations = args.max_iterations;
    opts.timeout = Duration::from_secs(args.timeout);
    opts.test_command = args.test_cmd.clone();
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
