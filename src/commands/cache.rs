use crate::error::CliError;
use crate::output::Output;
use crate::usage_store::{hit_rate, read_usage};
use clap::{Args, Subcommand};
use serde_json::json;

#[derive(Args, Debug)]
pub struct CacheArgs {
    #[command(subcommand)]
    pub sub: CacheSub,
}

#[derive(Subcommand, Debug)]
pub enum CacheSub {
    /// Show cumulative prompt-cache hit/miss token stats
    Stats {
        /// Show stats for one run/test task only
        #[arg(long)]
        task: Option<String>,
    },
}

pub fn run(args: &CacheArgs) -> Result<Output, CliError> {
    let CacheSub::Stats { task } = &args.sub;
    let usage = read_usage();

    if let Some(task_id) = task {
        let bucket = usage.by_task.get(task_id).ok_or_else(|| {
            CliError::validation(
                format!("No recorded usage for task: {task_id}"),
                vec!["Check the taskId — it's the suffix of the deepseek-axi/<id> branch name".into()],
            )
        })?;
        return Ok(Output::Data(json!({
            "task": task_id,
            "calls": bucket.calls,
            "hitTokens": bucket.hit_tokens,
            "missTokens": bucket.miss_tokens,
            "hitRate": hit_rate(bucket.hit_tokens, bucket.miss_tokens),
            "reasoningTokens": bucket.reasoning_tokens,
            "lastAt": bucket.last_at.clone().unwrap_or_else(|| "n/a".into()),
        })));
    }

    Ok(Output::Data(json!({
        "calls": usage.calls,
        "hitTokens": usage.hit_tokens,
        "missTokens": usage.miss_tokens,
        "hitRate": hit_rate(usage.hit_tokens, usage.miss_tokens),
        "reasoningTokens": usage.reasoning_tokens,
        "lastModel": usage.last_model.unwrap_or_else(|| "n/a".into()),
        "lastAt": usage.last_at.unwrap_or_else(|| "n/a".into()),
        "help": [
            "Cache hits are billed far cheaper than misses — see https://api-docs.deepseek.com/guides/kv_cache",
            "Run `deepseek-axi cache stats --task <id>` to see one run/test task's usage",
            "Stats reset by deleting ~/.config/deepseek-axi/usage.json",
        ],
    })))
}
