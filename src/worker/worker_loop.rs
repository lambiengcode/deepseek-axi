//! The worker state machine: discover -> plan/implement (agentic tool
//! turns) -> test -> diagnose/retry -> two-sided verify -> handoff. Every
//! dependency that touches the network, the filesystem, or git is
//! injectable (trait objects) so tests can drive this deterministically
//! without live model calls or a real DeepSeek API key — same design intent
//! as the Node build's dependency-injected `runWorkerLoop`.

use super::git_safety::{self, DiffSummary, WorktreeInfo};
use super::verify::{self, TwoSidedResult};
use crate::context_engine::{BuiltContext, build_context};
use crate::error::CliError;
use crate::handoff::{Cost, FileChanged, Handoff, Status, UsageTotals, Verification};
use crate::models::MODEL_PRO;
use crate::tools::{ToolContext, execute_tool_call, read_only_tool_defs, tool_defs};
use crate::usage_store::record_usage;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_TOOL_TURNS_PER_ITERATION: u32 = 8;

type BuildContextFn = dyn Fn(&str, &Path, &[String], usize, usize) -> (BuiltContext, String);
type RecordSharedUsageFn = dyn Fn(&str, Option<&Value>, String, Option<&str>);

// ---- Injectable dependency traits ----

pub trait GitSafetyOps {
    fn ensure_worktree(&self, cwd: &Path, task_id: &str) -> Result<WorktreeInfo, String>;
    fn checkpoint(&self, worktree_path: &Path, message: &str) -> Result<Option<String>, String>;
    fn diff_summary(&self, worktree_path: &Path, base_ref: &str) -> Result<DiffSummary, String>;
    fn discard_worktree(&self, cwd: &Path, task_id: &str);
    fn promote(&self, cwd: &Path, task_id: &str) -> Result<String, String>;
    fn current_head(&self, cwd: &Path) -> Result<String, String>;
}

pub struct RealGitSafety;
impl GitSafetyOps for RealGitSafety {
    fn ensure_worktree(&self, cwd: &Path, task_id: &str) -> Result<WorktreeInfo, String> {
        git_safety::ensure_worktree(cwd, task_id)
    }
    fn checkpoint(&self, worktree_path: &Path, message: &str) -> Result<Option<String>, String> {
        git_safety::checkpoint(worktree_path, message)
    }
    fn diff_summary(&self, worktree_path: &Path, base_ref: &str) -> Result<DiffSummary, String> {
        git_safety::diff_summary(worktree_path, base_ref)
    }
    fn discard_worktree(&self, cwd: &Path, task_id: &str) {
        git_safety::discard_worktree(cwd, task_id)
    }
    fn promote(&self, cwd: &Path, task_id: &str) -> Result<String, String> {
        git_safety::promote(cwd, task_id)
    }
    fn current_head(&self, cwd: &Path) -> Result<String, String> {
        git_safety::current_head(cwd)
    }
}

pub trait VerifyOps {
    fn run_tests(&self, cwd: &Path, test_command: Option<&str>) -> verify::TestResult;
    fn verify_two_sided(
        &self,
        cwd: &Path,
        worktree_path: &Path,
        base_ref: &str,
        test_command: Option<&str>,
        task_id: &str,
    ) -> TwoSidedResult;
    fn verify_single_sided(&self, worktree_path: &Path, test_command: Option<&str>) -> TwoSidedResult;
}

pub struct RealVerify;
impl VerifyOps for RealVerify {
    fn run_tests(&self, cwd: &Path, test_command: Option<&str>) -> verify::TestResult {
        verify::run_tests(cwd, test_command)
    }
    fn verify_two_sided(
        &self,
        cwd: &Path,
        worktree_path: &Path,
        base_ref: &str,
        test_command: Option<&str>,
        task_id: &str,
    ) -> TwoSidedResult {
        verify::verify_two_sided(cwd, worktree_path, base_ref, test_command, task_id)
    }
    fn verify_single_sided(&self, worktree_path: &Path, test_command: Option<&str>) -> TwoSidedResult {
        verify::verify_single_sided(worktree_path, test_command)
    }
}

// ---- Options ----

pub struct LoopOptions {
    pub task_id: String,
    pub task: String,
    pub cwd: PathBuf,
    pub files: Vec<String>,
    pub model: String,
    pub max_files: usize,
    pub max_iterations: u32,
    pub max_cost_usd: Option<f64>,
    pub timeout: Duration,
    pub test_command: Option<String>,
    pub skip_regression_check: bool,
    pub dry_run: bool,
    pub use_worktree: bool,
    pub chat_completion_fn: Box<dyn Fn(Value) -> Result<Value, CliError>>,
    /// FIM completion (`/beta/completions`), passed through to the
    /// `fim_edit` tool — kept separate from `chat_completion_fn` since it's
    /// a different DeepSeek endpoint with a different request shape.
    pub completion_fn: Box<dyn Fn(Value) -> Result<Value, CliError>>,
    pub git_safety: Box<dyn GitSafetyOps>,
    pub verify: Box<dyn VerifyOps>,
    pub build_context_fn: Box<BuildContextFn>,
    pub record_shared_usage_fn: Box<RecordSharedUsageFn>,
}

impl LoopOptions {
    pub fn real(task_id: String, task: String, cwd: PathBuf) -> Self {
        Self {
            task_id,
            task,
            cwd,
            files: Vec::new(),
            model: MODEL_PRO.to_string(),
            max_files: 20,
            max_iterations: 6,
            max_cost_usd: None,
            timeout: Duration::from_secs(600),
            test_command: None,
            skip_regression_check: false,
            dry_run: false,
            use_worktree: true,
            chat_completion_fn: Box::new(crate::api::chat_completion),
            completion_fn: Box::new(crate::api::completion),
            git_safety: Box::new(RealGitSafety),
            verify: Box::new(RealVerify),
            build_context_fn: Box::new(build_context),
            record_shared_usage_fn: Box::new(|model, usage, ts, task_id| {
                record_usage(model, usage, ts, task_id);
            }),
        }
    }
}

struct BudgetExceeded(String);

enum LoopStop {
    Budget(String),
    Error(CliError),
}

fn estimate_cost_usd(completion_tokens: u64, model: &str) -> f64 {
    let per_million_output = if model == MODEL_PRO { 0.87 } else { 0.28 };
    (completion_tokens as f64 / 1_000_000.0) * per_million_output
}

enum Phase<'a> {
    PlanOnly,
    Implement,
    Diagnose(&'a str),
}

fn build_instructions(context_prompt: &str, phase: Phase) -> String {
    let mut parts = vec![context_prompt.to_string()];
    match phase {
        Phase::PlanOnly => parts.push(
            "You are in PLAN-ONLY mode. Use read_file/grep/list_files to understand the codebase, \
             then describe your plan in your final text response. write_file and run_tests are not \
             available in this mode."
                .to_string(),
        ),
        Phase::Diagnose(previous_failure) => parts.push(format!(
            "The previous attempt's tests failed with this output:\n{}\n\nDiagnose the failure and fix it using read_file/grep, and fim_edit or write_file as needed. \
             Prefer fim_edit for a small, localized fix.",
            truncate(previous_failure, 4000)
        )),
        Phase::Implement => parts.push(
            "Implement this task using read_file/grep/list_files to gather context. To apply changes, \
             prefer fim_edit for a small, localized fix confined to a few lines (it replaces only that \
             line range and leaves the rest of the file untouched); use write_file only for a new file \
             or a change spanning most of the file. Make the smallest correct change. Summarize what \
             you changed in your final text response once done."
                .to_string(),
        ),
    }
    parts.join("\n\n")
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[allow(clippy::too_many_arguments)]
fn run_agentic_turn(
    instructions: &str,
    worktree_path: &Path,
    model: &str,
    chat_completion_fn: &dyn Fn(Value) -> Result<Value, CliError>,
    completion_fn: &dyn Fn(Value) -> Result<Value, CliError>,
    test_command: Option<&str>,
    record_call_usage: &dyn Fn(&Value),
    started_at: Instant,
    timeout: Duration,
    iterations: u32,
    max_iterations: u32,
    completion_tokens_so_far: u64,
    max_cost_usd: Option<f64>,
    model_for_cost: &str,
    allow_write: bool,
) -> Result<String, LoopStop> {
    let ctx = ToolContext {
        cwd: worktree_path.to_path_buf(),
        test_command: test_command.map(str::to_string),
        allow_write,
        completion_fn,
    };
    let tools = if allow_write {
        tool_defs()
    } else {
        read_only_tool_defs()
    };

    let mut messages: Vec<Value> = vec![json!({"role": "user", "content": instructions})];
    let mut final_text = String::new();

    for _ in 0..MAX_TOOL_TURNS_PER_ITERATION {
        check_budget(
            started_at,
            timeout,
            iterations,
            max_iterations,
            completion_tokens_so_far,
            max_cost_usd,
            model_for_cost,
        )
        .map_err(|BudgetExceeded(m)| LoopStop::Budget(m))?;

        let payload = json!({
            "model": model,
            "messages": messages,
            "tools": tools,
            "thinking": {"type": "enabled"},
            "reasoning_effort": "high",
        });

        let response = chat_completion_fn(payload).map_err(LoopStop::Error)?;
        if let Some(usage) = response.get("usage") {
            record_call_usage(usage);
        }

        let message = response
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .cloned()
            .unwrap_or(json!({}));
        let tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        messages.push(json!({
            "role": "assistant",
            "content": message.get("content").and_then(Value::as_str).unwrap_or(""),
            "tool_calls": message.get("tool_calls"),
        }));

        if tool_calls.is_empty() {
            final_text = message.get("content").and_then(Value::as_str).unwrap_or("").to_string();
            break;
        }

        for call in &tool_calls {
            let name = call["function"]["name"].as_str().unwrap_or("");
            let args_str = call["function"]["arguments"].as_str().unwrap_or("{}");
            let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
            let result = execute_tool_call(name, &args, &ctx);
            messages.push(json!({"role": "tool", "tool_call_id": call.get("id"), "content": result.to_string()}));
        }
    }

    Ok(final_text)
}

fn check_budget(
    started_at: Instant,
    timeout: Duration,
    iterations: u32,
    max_iterations: u32,
    completion_tokens: u64,
    max_cost_usd: Option<f64>,
    model: &str,
) -> Result<(), BudgetExceeded> {
    if started_at.elapsed() > timeout {
        return Err(BudgetExceeded(format!("timeout after {}ms", timeout.as_millis())));
    }
    if iterations > max_iterations {
        return Err(BudgetExceeded(format!("exceeded --max-iterations ({max_iterations})")));
    }
    if let Some(max_cost) = max_cost_usd
        && estimate_cost_usd(completion_tokens, model) > max_cost
    {
        return Err(BudgetExceeded(format!("exceeded --max-cost (${max_cost})")));
    }
    Ok(())
}

fn build_next_actions(status: &Status, branch: Option<&str>) -> Vec<String> {
    match status {
        Status::Verified => branch
            .map(|b| vec![format!("Review the diff and merge/PR branch {b} when ready")])
            .unwrap_or_default(),
        Status::PartiallyVerified => vec!["Review remainingWork and the diff manually before merging".to_string()],
        Status::Failed => vec![
            "Review the last test output in remainingWork; consider a narrower task or manual intervention".to_string(),
        ],
        Status::Implemented => vec![],
    }
}

/// Runs the full worker loop. Returns a `Handoff` on every path — even a
/// budget-exceeded stop returns a (partial) handoff rather than an error,
/// matching the Node build's contract.
pub fn run_worker_loop(opts: LoopOptions) -> Result<Handoff, CliError> {
    let started_at = Instant::now();
    // RefCell, not a plain mutable capture: `record_call_usage` needs to be
    // called from inside `run_agentic_turn`'s loop while budget checks in
    // the *same* turn also need to read the just-updated total (a real bug
    // caught by the borrow checker — a plain `&mut usage` closure couldn't
    // be read from elsewhere while alive, which would have meant mid-turn
    // budget checks silently used a stale completion_tokens count).
    let usage = RefCell::new(UsageTotals::default());
    let mut iterations: u32 = 0;

    let record_call_usage = |u: &Value| {
        let mut u_mut = usage.borrow_mut();
        u_mut.prompt_tokens += u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
        u_mut.completion_tokens += u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0);
        u_mut.cache_hit_tokens += u.get("prompt_cache_hit_tokens").and_then(Value::as_u64).unwrap_or(0);
        u_mut.cache_miss_tokens += u.get("prompt_cache_miss_tokens").and_then(Value::as_u64).unwrap_or(0);
        drop(u_mut);
        (opts.record_shared_usage_fn)(&opts.model, Some(u), crate::time_util::now_iso(), Some(&opts.task_id));
    };

    let (worktree_path, branch, base_ref) = if opts.use_worktree {
        let info = opts
            .git_safety
            .ensure_worktree(&opts.cwd, &opts.task_id)
            .map_err(|e| CliError::new(format!("git worktree setup failed: {e}"), "API_ERROR", vec![]))?;
        (info.path, Some(info.branch), info.base_ref)
    } else {
        let head = opts
            .git_safety
            .current_head(&opts.cwd)
            .map_err(|e| CliError::new(format!("git rev-parse HEAD failed: {e}"), "API_ERROR", vec![]))?;
        (opts.cwd.clone(), None, head)
    };

    let (context, prompt) = (opts.build_context_fn)(&opts.task, &worktree_path, &opts.files, 8000, opts.max_files);

    if opts.dry_run {
        // Snapshot into a local first: `usage.borrow().completion_tokens`
        // inline as an argument keeps its Ref guard alive for the whole
        // call statement (Rust's temporary-lifetime-extension rule) — since
        // run_agentic_turn calls back into record_call_usage's borrow_mut()
        // internally, an inline borrow here would panic with "already
        // borrowed". A real bug caught by the test suite, not theoretical.
        let completion_tokens_snapshot = usage.borrow().completion_tokens;
        let turn_result = run_agentic_turn(
            &build_instructions(&prompt, Phase::PlanOnly),
            &worktree_path,
            &opts.model,
            &*opts.chat_completion_fn,
            &*opts.completion_fn,
            opts.test_command.as_deref(),
            &record_call_usage,
            started_at,
            opts.timeout,
            1,
            opts.max_iterations,
            completion_tokens_snapshot,
            opts.max_cost_usd,
            &opts.model,
            false,
        );
        iterations = 1;
        if opts.use_worktree {
            opts.git_safety.discard_worktree(&opts.cwd, &opts.task_id);
        }

        return match turn_result {
            Ok(plan_text) => {
                let mut h = Handoff::builder(
                    Status::Implemented,
                    if plan_text.is_empty() {
                        "(dry run — no summary returned)".into()
                    } else {
                        plan_text
                    },
                );
                h.risks = vec!["--dry-run: no files were written, no tests were run".into()];
                h.remaining_work = vec!["Run without --dry-run to apply and verify this plan".into()];
                h.next_actions = vec!["Review the plan; re-run without --dry-run to execute it".into()];
                h.cost = Cost {
                    iterations,
                    model: Some(opts.model.clone()),
                };
                h.usage = usage.borrow().clone();
                Ok(h)
            }
            Err(LoopStop::Budget(msg)) => Ok(budget_exceeded_handoff(
                msg,
                iterations,
                &opts.model,
                usage.borrow().clone(),
                None,
            )),
            Err(LoopStop::Error(e)) => Err(e),
        };
    }

    #[allow(unused_assignments)] // always overwritten before the loop's first read, on every path
    let mut last_turn_text = String::new();
    let mut test_result: Option<verify::TestResult> = None;

    loop {
        iterations += 1;
        let completion_tokens_snapshot = usage.borrow().completion_tokens;
        if let Err(BudgetExceeded(msg)) = check_budget(
            started_at,
            opts.timeout,
            iterations,
            opts.max_iterations,
            completion_tokens_snapshot,
            opts.max_cost_usd,
            &opts.model,
        ) {
            let branch_out = preserve_partial_work(&opts, branch.as_deref());
            return Ok(budget_exceeded_handoff(
                msg,
                iterations,
                &opts.model,
                usage.borrow().clone(),
                branch_out,
            ));
        }

        let phase = if iterations == 1 {
            Phase::Implement
        } else {
            match &test_result {
                Some(r) if !r.passed => Phase::Diagnose(&r.output),
                _ => Phase::Implement,
            }
        };
        let instructions = build_instructions(&prompt, phase);
        let completion_tokens_snapshot = usage.borrow().completion_tokens;

        let turn_result = run_agentic_turn(
            &instructions,
            &worktree_path,
            &opts.model,
            &*opts.chat_completion_fn,
            &*opts.completion_fn,
            opts.test_command.as_deref(),
            &record_call_usage,
            started_at,
            opts.timeout,
            iterations,
            opts.max_iterations,
            completion_tokens_snapshot,
            opts.max_cost_usd,
            &opts.model,
            true,
        );

        match turn_result {
            Ok(text) => last_turn_text = text,
            Err(LoopStop::Budget(msg)) => {
                let branch_out = preserve_partial_work(&opts, branch.as_deref());
                return Ok(budget_exceeded_handoff(
                    msg,
                    iterations,
                    &opts.model,
                    usage.borrow().clone(),
                    branch_out,
                ));
            }
            Err(LoopStop::Error(e)) => {
                if opts.use_worktree {
                    opts.git_safety.discard_worktree(&opts.cwd, &opts.task_id);
                }
                return Err(e);
            }
        }

        let _ = opts.git_safety.checkpoint(
            &worktree_path,
            &format!("deepseek-axi iteration {iterations}: {}", truncate(&opts.task, 72)),
        );

        let result = opts.verify.run_tests(&worktree_path, opts.test_command.as_deref());
        let passed = result.passed;
        test_result = Some(result);
        if passed || iterations >= opts.max_iterations {
            break;
        }
    }

    let verification = if opts.skip_regression_check {
        opts.verify
            .verify_single_sided(&worktree_path, opts.test_command.as_deref())
    } else {
        opts.verify.verify_two_sided(
            &opts.cwd,
            &worktree_path,
            &base_ref,
            opts.test_command.as_deref(),
            &opts.task_id,
        )
    };

    let diff = opts
        .git_safety
        .diff_summary(&worktree_path, &base_ref)
        .unwrap_or(DiffSummary {
            diff: String::new(),
            truncated: false,
            files_changed: vec![],
        });

    let status = if verification.target_fixed && verification.no_regressions {
        Status::Verified
    } else if !diff.files_changed.is_empty() {
        Status::PartiallyVerified
    } else {
        Status::Failed
    };

    let mut risks = Vec::new();
    if opts.skip_regression_check {
        risks.push("regression check skipped (--skip-regression-check)".to_string());
    }
    if verification.reproduced_on_base == Some(false) {
        risks.push(
            "target failure did not reproduce on the base commit — the fix may not have been necessary".to_string(),
        );
    }
    if !context.dropped.is_empty() {
        risks.push(format!(
            "context budget exceeded — dropped: {}",
            context.dropped.join(", ")
        ));
    }

    let mut remaining_work = Vec::new();
    if !matches!(status, Status::Verified) && !verification.post.output.is_empty() {
        remaining_work.push(format!(
            "Last test output: {}",
            truncate(&verification.post.output, 500)
        ));
    }

    let mut branch_out: Option<String> = None;
    if opts.use_worktree {
        if matches!(status, Status::Verified | Status::PartiallyVerified) {
            if let Ok(b) = opts.git_safety.promote(&opts.cwd, &opts.task_id) {
                branch_out = Some(b);
            }
        } else {
            opts.git_safety.discard_worktree(&opts.cwd, &opts.task_id);
        }
    }

    let mut h = Handoff::builder(
        status.clone(),
        if last_turn_text.is_empty() {
            format!("Worker loop finished with status {status:?} after {iterations} iteration(s).")
        } else {
            last_turn_text
        },
    );
    h.files_changed = diff
        .files_changed
        .into_iter()
        .map(|f| FileChanged {
            path: f.path,
            insertions: f.insertions,
            deletions: f.deletions,
        })
        .collect();
    h.tests.run = if verification.post.ran { 1 } else { 0 };
    h.tests.passed = if verification.post.passed { 1 } else { 0 };
    h.tests.failed = if verification.post.passed { 0 } else { 1 };
    h.verification = Verification {
        target_fixed: verification.target_fixed,
        no_regressions: verification.no_regressions,
        regression_check_skipped: verification.regression_check_skipped,
    };
    h.risks = risks;
    h.remaining_work = remaining_work;
    h.next_actions = build_next_actions(&status, branch_out.as_deref());
    h.cost = Cost {
        iterations,
        model: Some(opts.model.clone()),
    };
    h.usage = usage.into_inner();
    h.branch = branch_out;

    Ok(h)
}

fn preserve_partial_work(opts: &LoopOptions, branch: Option<&str>) -> Option<String> {
    if !opts.use_worktree {
        return None;
    }
    opts.git_safety
        .promote(&opts.cwd, &opts.task_id)
        .ok()
        .or_else(|| branch.map(str::to_string))
}

fn budget_exceeded_handoff(
    msg: String,
    iterations: u32,
    model: &str,
    usage: UsageTotals,
    branch: Option<String>,
) -> Handoff {
    let mut h = Handoff::builder(Status::Failed, format!("Stopped early: {msg}"));
    h.risks = vec![format!("budget exceeded: {msg}")];
    h.remaining_work = match &branch {
        Some(b) => vec![format!(
            "Partial work is on branch {b} — inspect or continue it directly"
        )],
        None => vec!["Re-run with a higher budget flag, or narrow the task".to_string()],
    };
    h.next_actions = match &branch {
        Some(b) => vec![
            format!("Inspect partial work on branch {b}"),
            "Increase --max-iterations/--max-cost/--timeout and retry".to_string(),
        ],
        None => vec!["Increase --max-iterations/--max-cost/--timeout and retry".to_string()],
    };
    h.cost = Cost {
        iterations,
        model: Some(model.to_string()),
    };
    h.usage = usage;
    h.branch = branch;
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    struct StubGitSafetyInner {
        ensure_worktree_calls: Cell<u32>,
        checkpoint_calls: RefCell<Vec<String>>,
        discard_calls: Cell<u32>,
        promote_calls: Cell<u32>,
    }
    impl StubGitSafetyInner {
        fn new() -> Rc<Self> {
            Rc::new(Self {
                ensure_worktree_calls: Cell::new(0),
                checkpoint_calls: RefCell::new(Vec::new()),
                discard_calls: Cell::new(0),
                promote_calls: Cell::new(0),
            })
        }
    }
    impl GitSafetyOps for Rc<StubGitSafetyInner> {
        fn ensure_worktree(&self, _cwd: &Path, task_id: &str) -> Result<WorktreeInfo, String> {
            self.ensure_worktree_calls.set(self.ensure_worktree_calls.get() + 1);
            Ok(WorktreeInfo {
                path: PathBuf::from(format!("/tmp/wt-{task_id}")),
                branch: format!("deepseek-axi/{task_id}"),
                base_ref: "base-sha".into(),
            })
        }
        fn checkpoint(&self, _worktree_path: &Path, message: &str) -> Result<Option<String>, String> {
            self.checkpoint_calls.borrow_mut().push(message.to_string());
            Ok(Some("sha".into()))
        }
        fn diff_summary(&self, _worktree_path: &Path, _base_ref: &str) -> Result<DiffSummary, String> {
            Ok(DiffSummary {
                diff: "diff --git a b".into(),
                truncated: false,
                files_changed: vec![git_safety::FileChangedRow {
                    path: "a.js".into(),
                    insertions: 1,
                    deletions: 0,
                }],
            })
        }
        fn discard_worktree(&self, _cwd: &Path, _task_id: &str) {
            self.discard_calls.set(self.discard_calls.get() + 1);
        }
        fn promote(&self, _cwd: &Path, task_id: &str) -> Result<String, String> {
            self.promote_calls.set(self.promote_calls.get() + 1);
            Ok(format!("deepseek-axi/{task_id}"))
        }
        fn current_head(&self, _cwd: &Path) -> Result<String, String> {
            Ok("base-sha".into())
        }
    }

    enum VerifyBehavior {
        AlwaysPass,
        AlwaysFail,
        PassOnNthTestRun(u32),
    }
    struct StubVerifyInner {
        behavior: VerifyBehavior,
        test_run_count: Cell<u32>,
        two_sided_called: Cell<bool>,
        single_sided_called: Cell<bool>,
    }
    impl StubVerifyInner {
        fn new(behavior: VerifyBehavior) -> Rc<Self> {
            Rc::new(Self {
                behavior,
                test_run_count: Cell::new(0),
                two_sided_called: Cell::new(false),
                single_sided_called: Cell::new(false),
            })
        }
    }
    fn passes_for(behavior: &VerifyBehavior, run_count: u32) -> bool {
        match behavior {
            VerifyBehavior::AlwaysPass => true,
            VerifyBehavior::AlwaysFail => false,
            VerifyBehavior::PassOnNthTestRun(n) => run_count >= *n,
        }
    }
    impl VerifyOps for Rc<StubVerifyInner> {
        fn run_tests(&self, _cwd: &Path, _test_command: Option<&str>) -> verify::TestResult {
            self.test_run_count.set(self.test_run_count.get() + 1);
            let passed = passes_for(&self.behavior, self.test_run_count.get());
            verify::TestResult {
                ran: true,
                passed,
                output: if passed { String::new() } else { "still failing".into() },
            }
        }
        fn verify_two_sided(
            &self,
            _cwd: &Path,
            _worktree_path: &Path,
            _base_ref: &str,
            _test_command: Option<&str>,
            _task_id: &str,
        ) -> TwoSidedResult {
            self.two_sided_called.set(true);
            let passed = passes_for(&self.behavior, self.test_run_count.get());
            TwoSidedResult {
                post: verify::TestResult {
                    ran: true,
                    passed,
                    output: if passed { String::new() } else { "still failing".into() },
                },
                pre: Some(verify::TestResult {
                    ran: true,
                    passed: false,
                    output: String::new(),
                }),
                target_fixed: passed,
                no_regressions: passed,
                reproduced_on_base: Some(true),
                regression_check_skipped: false,
            }
        }
        fn verify_single_sided(&self, _worktree_path: &Path, _test_command: Option<&str>) -> TwoSidedResult {
            self.single_sided_called.set(true);
            let passed = passes_for(&self.behavior, self.test_run_count.get());
            TwoSidedResult {
                post: verify::TestResult {
                    ran: true,
                    passed,
                    output: String::new(),
                },
                pre: None,
                target_fixed: passed,
                no_regressions: passed,
                reproduced_on_base: None,
                regression_check_skipped: true,
            }
        }
    }

    fn stub_model(text: &'static str, completion_tokens: u64) -> Box<dyn Fn(Value) -> Result<Value, CliError>> {
        Box::new(move |_payload: Value| {
            Ok(json!({
                "choices": [{"message": {"content": text}}],
                "usage": {"prompt_tokens": 10, "completion_tokens": completion_tokens}
            }))
        })
    }

    fn stub_build_context() -> Box<BuildContextFn> {
        Box::new(|_task, _cwd, _files, _budget, _max_files| {
            (
                BuiltContext {
                    branch: None,
                    touched_files: vec![],
                    related_files: vec![],
                    test_files: vec![],
                    tokens_estimate: 10,
                    included: vec![],
                    dropped: vec![],
                    prompt: None,
                },
                "context prompt".to_string(),
            )
        })
    }

    fn base_opts(git: Rc<StubGitSafetyInner>, verify: Rc<StubVerifyInner>) -> LoopOptions {
        LoopOptions {
            task_id: "t1".into(),
            task: "fix the bug".into(),
            cwd: PathBuf::from("/repo"),
            files: vec![],
            model: MODEL_PRO.to_string(),
            max_files: 20,
            max_iterations: 6,
            max_cost_usd: None,
            timeout: Duration::from_secs(600),
            test_command: None,
            skip_regression_check: false,
            dry_run: false,
            use_worktree: true,
            chat_completion_fn: stub_model("did the thing", 10),
            completion_fn: Box::new(|_| Ok(json!({"choices": [{"text": ""}]}))),
            git_safety: Box::new(git),
            verify: Box::new(verify),
            build_context_fn: stub_build_context(),
            record_shared_usage_fn: Box::new(|_, _, _, _| {}),
        }
    }

    #[test]
    fn happy_path_one_iteration_verified_worktree_promoted() {
        let git = StubGitSafetyInner::new();
        let verify = StubVerifyInner::new(VerifyBehavior::AlwaysPass);
        let opts = base_opts(git.clone(), verify.clone());

        let handoff = run_worker_loop(opts).unwrap();
        assert_eq!(handoff.status, Status::Verified);
        assert_eq!(handoff.cost.iterations, 1);
        assert_eq!(git.checkpoint_calls.borrow().len(), 1);
        assert_eq!(git.promote_calls.get(), 1);
        assert_eq!(git.discard_calls.get(), 0);
        assert_eq!(handoff.branch, Some("deepseek-axi/t1".to_string()));
    }

    #[test]
    fn retries_after_failing_test_and_succeeds_on_second_iteration() {
        let git = StubGitSafetyInner::new();
        let verify = StubVerifyInner::new(VerifyBehavior::PassOnNthTestRun(2));
        let mut opts = base_opts(git.clone(), verify.clone());
        opts.max_iterations = 5;

        let handoff = run_worker_loop(opts).unwrap();
        assert_eq!(handoff.status, Status::Verified);
        assert_eq!(handoff.cost.iterations, 2);
    }

    #[test]
    fn exhausts_max_iterations_without_passing_partially_verified() {
        let git = StubGitSafetyInner::new();
        let verify = StubVerifyInner::new(VerifyBehavior::AlwaysFail);
        let mut opts = base_opts(git.clone(), verify.clone());
        opts.max_iterations = 2;

        let handoff = run_worker_loop(opts).unwrap();
        assert_eq!(handoff.status, Status::PartiallyVerified);
        assert_eq!(handoff.cost.iterations, 2);
        assert_eq!(git.promote_calls.get(), 1);
        assert!(!handoff.remaining_work.is_empty());
    }

    #[test]
    fn dry_run_never_writes_never_tests_discards_worktree() {
        let git = StubGitSafetyInner::new();
        let verify = StubVerifyInner::new(VerifyBehavior::AlwaysPass);
        let mut opts = base_opts(git.clone(), verify.clone());
        opts.dry_run = true;
        opts.chat_completion_fn = Box::new(|payload: Value| {
            let tools = payload
                .get("tools")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let names: Vec<String> = tools
                .iter()
                .map(|t| t["function"]["name"].as_str().unwrap_or("").to_string())
                .collect();
            assert!(!names.contains(&"write_file".to_string()));
            assert!(!names.contains(&"run_tests".to_string()));
            assert!(!names.contains(&"fim_edit".to_string()));
            Ok(
                json!({"choices": [{"message": {"content": "here is my plan"}}], "usage": {"prompt_tokens": 5, "completion_tokens": 5}}),
            )
        });

        let handoff = run_worker_loop(opts).unwrap();
        assert_eq!(handoff.status, Status::Implemented);
        assert_eq!(handoff.summary, "here is my plan");
        assert_eq!(git.checkpoint_calls.borrow().len(), 0);
        assert_eq!(git.discard_calls.get(), 1);
        assert_eq!(git.promote_calls.get(), 0);
        assert_eq!(verify.test_run_count.get(), 0);
    }

    #[test]
    fn exceeding_max_cost_stops_early_and_preserves_partial_work_via_promote() {
        let git = StubGitSafetyInner::new();
        let verify = StubVerifyInner::new(VerifyBehavior::AlwaysFail);
        let mut opts = base_opts(git.clone(), verify.clone());
        opts.max_iterations = 10;
        opts.max_cost_usd = Some(0.1);
        opts.chat_completion_fn = stub_model("...", 1_000_000); // ~$0.87 on pro, over budget after 1 call

        let handoff = run_worker_loop(opts).unwrap();
        assert_eq!(handoff.status, Status::Failed);
        assert!(handoff.summary.starts_with("Stopped early"));
        assert!(handoff.risks[0].contains("budget exceeded"));
        assert_eq!(handoff.branch, Some("deepseek-axi/t1".to_string()));
        assert_eq!(git.promote_calls.get(), 1);
        assert_eq!(git.discard_calls.get(), 0);
    }

    #[test]
    fn skip_regression_check_routes_to_verify_single_sided() {
        let git = StubGitSafetyInner::new();
        let verify = StubVerifyInner::new(VerifyBehavior::AlwaysPass);
        let mut opts = base_opts(git.clone(), verify.clone());
        opts.skip_regression_check = true;

        let handoff = run_worker_loop(opts).unwrap();
        assert!(verify.single_sided_called.get());
        assert!(!verify.two_sided_called.get());
        assert!(handoff.verification.regression_check_skipped);
        assert!(handoff.risks.iter().any(|r| r.contains("regression check skipped")));
    }

    #[test]
    fn use_worktree_false_operates_directly_on_cwd() {
        let git = StubGitSafetyInner::new();
        let verify = StubVerifyInner::new(VerifyBehavior::AlwaysPass);
        let mut opts = base_opts(git.clone(), verify.clone());
        opts.use_worktree = false;

        let handoff = run_worker_loop(opts).unwrap();
        assert_eq!(git.ensure_worktree_calls.get(), 0);
        assert_eq!(handoff.status, Status::Verified);
        assert_eq!(handoff.branch, None);
    }

    #[test]
    fn executes_a_model_tool_call_and_continues_the_conversation() {
        let git = StubGitSafetyInner::new();
        let verify = StubVerifyInner::new(VerifyBehavior::AlwaysPass);
        let mut opts = base_opts(git.clone(), verify.clone());

        let call_count = Rc::new(Cell::new(0));
        let call_count_clone = call_count.clone();
        opts.chat_completion_fn = Box::new(move |payload: Value| {
            let n = call_count_clone.get() + 1;
            call_count_clone.set(n);
            if n == 1 {
                Ok(json!({
                    "choices": [{"message": {"content": "", "tool_calls": [{"id": "c1", "function": {"name": "read_file", "arguments": "{\"path\":\"a.js\"}"}}]}}],
                    "usage": {"prompt_tokens": 5, "completion_tokens": 5}
                }))
            } else {
                let messages = payload
                    .get("messages")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let tool_msg = messages.iter().find(|m| m["role"] == "tool");
                assert!(
                    tool_msg.is_some(),
                    "expected the tool result to be threaded back into the conversation"
                );
                Ok(
                    json!({"choices": [{"message": {"content": "done reading, made the fix"}}], "usage": {"prompt_tokens": 5, "completion_tokens": 5}}),
                )
            }
        });

        let handoff = run_worker_loop(opts).unwrap();
        assert_eq!(handoff.status, Status::Verified);
        assert_eq!(handoff.summary, "done reading, made the fix");
        assert_eq!(call_count.get(), 2);
    }
}
