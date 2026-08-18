#![allow(dead_code)] // TwoSidedResult.pre: kept for diagnostics/handoff detail, not read by every caller

use super::git_safety::{checkout_scratch, remove_scratch};
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const NO_TEST_SCRIPT: &str = "echo \"Error: no test specified\" && exit 1";

fn detect_npm(cwd: &Path) -> Option<String> {
    let text = std::fs::read_to_string(cwd.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let test_script = json.get("scripts")?.get("test")?.as_str()?;
    if test_script == NO_TEST_SCRIPT {
        return None;
    }
    Some("npm test".to_string())
}

/// Detects the project's test command by convention. Checked in order, so a
/// polyglot repo resolves to the first ecosystem it matches; `--test-cmd`
/// overrides this entirely.
///
/// Previously this recognised npm only, which meant that on a Rust, Python,
/// or Go repo the worker tier could not run tests at all — and the `run_tests`
/// *tool* separately hardcoded its own `npm test` fallback, so the two paths
/// disagreed with each other.
pub fn detect_test_command(cwd: &Path) -> Option<String> {
    if let Some(cmd) = detect_npm(cwd) {
        return Some(cmd);
    }
    if cwd.join("Cargo.toml").exists() {
        return Some("cargo test".to_string());
    }
    if cwd.join("pyproject.toml").exists() || cwd.join("pytest.ini").exists() || cwd.join("tox.ini").exists() {
        return Some("pytest".to_string());
    }
    if cwd.join("go.mod").exists() {
        return Some("go test ./...".to_string());
    }
    None
}

pub struct TestResult {
    pub ran: bool,
    pub passed: bool,
    pub output: String,
    /// The command hit its deadline and was killed. Distinct from a plain
    /// failure: a timeout says nothing about whether the code is correct.
    pub timed_out: bool,
}

/// Fallback deadline for a test run when the caller has no budget to give.
pub const DEFAULT_TEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Kills the whole process group, not just the direct child.
///
/// The child is `sh -c "<command>"`, so killing it alone orphans whatever it
/// spawned (`npm test` → node, `cargo test` → the test binaries) and those
/// keep the pipes open and the machine busy. `sh` is made a process-group
/// leader at spawn, so a negative PID reaches every descendant. Shelling out
/// to `kill` avoids taking a `libc` dependency for one call.
fn kill_process_group(child: &mut Child) {
    let pid = child.id();
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .arg("-KILL")
            .arg(format!("-{pid}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    // Always also kill the direct child: on non-unix, and on unix as a
    // backstop if the group kill did not land.
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs one command under a deadline, returning `(status_success, output,
/// timed_out)`.
///
/// Two things here are load-bearing and easy to get wrong:
///
/// 1. **The pipes are drained on their own threads.** Polling `try_wait()`
///    while leaving stdout/stderr undrained deadlocks as soon as a chatty
///    suite fills the OS pipe buffer — the child blocks writing, so it never
///    exits, so `try_wait` never reports it done.
/// 2. **The child leads its own process group**, so the timeout can reach the
///    grandchildren that actually do the work.
fn run_under_deadline(cwd: &Path, command: &str, timeout: Duration) -> Result<(bool, String, bool), String> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let mut stdout = child.stdout.take().ok_or("could not capture stdout")?;
    let mut stderr = child.stderr.take().ok_or("could not capture stderr")?;
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut success = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                success = status.success();
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    kill_process_group(&mut child);
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    }

    // Safe to join now: the child is gone either way, so both pipes are at EOF.
    let out = out_reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    let mut output = String::from_utf8_lossy(&out).into_owned();
    output.push_str(&String::from_utf8_lossy(&err));
    if timed_out {
        output.push_str(&format!(
            "\n\n[deepseek-axi] test command exceeded its {}s deadline and was killed.",
            timeout.as_secs()
        ));
    }
    Ok((success, output, timed_out))
}

/// Runs the test command once under `timeout`, classifying pass/fail at
/// whole-suite granularity. Raw failure output is returned as-is for the
/// model to diagnose — deliberately not parsed per test framework.
///
/// The deadline is not optional: a test command that never returns previously
/// blocked the entire run forever, because `--timeout` is only evaluated
/// between model turns and a blocking wait cannot be interrupted by it.
pub fn run_tests(cwd: &Path, test_command: Option<&str>, timeout: Duration) -> TestResult {
    let command = test_command.map(str::to_string).or_else(|| detect_test_command(cwd));
    let Some(command) = command else {
        return TestResult {
            ran: false,
            passed: false,
            output: "No test command detected or provided".into(),
            timed_out: false,
        };
    };

    match run_under_deadline(cwd, &command, timeout) {
        Ok((success, output, timed_out)) => TestResult {
            ran: true,
            passed: success && !timed_out,
            output,
            timed_out,
        },
        Err(e) => TestResult {
            ran: true,
            passed: false,
            output: e,
            timed_out: false,
        },
    }
}

pub struct TwoSidedResult {
    pub post: TestResult,
    pub pre: Option<TestResult>,
    pub target_fixed: bool,
    pub no_regressions: bool,
    pub reproduced_on_base: Option<bool>,
    pub regression_check_skipped: bool,
}

/// Runs the suite at `base_ref` in a throwaway detached worktree — the
/// "did this failure actually reproduce before the fix?" half of two-sided
/// verification. Depends only on `base_ref`, never on the model's work.
pub fn run_base_tests(
    cwd: &Path,
    base_ref: &str,
    task_id: &str,
    test_command: Option<&str>,
    timeout: Duration,
) -> TestResult {
    match checkout_scratch(cwd, base_ref, task_id) {
        Ok(scratch_path) => {
            let result = run_tests(&scratch_path, test_command, timeout);
            remove_scratch(cwd, task_id);
            result
        }
        Err(e) => TestResult {
            ran: false,
            passed: false,
            output: e,
            timed_out: false,
        },
    }
}

/// A base-commit test run executing on a background thread.
///
/// The scratch worktree is created **synchronously** before the thread
/// starts, so no two git commands ever run against the repository at once —
/// only the test process itself overlaps with the caller's work.
pub struct PendingBaseTests {
    handle: std::thread::JoinHandle<TestResult>,
}

impl PendingBaseTests {
    pub fn join(self, cwd: &Path, task_id: &str) -> TestResult {
        let result = self.handle.join().unwrap_or_else(|_| TestResult {
            ran: false,
            passed: false,
            output: "base-commit test run panicked".to_string(),
            timed_out: false,
        });
        remove_scratch(cwd, task_id);
        result
    }
}

pub fn spawn_base_tests(
    cwd: &Path,
    base_ref: &str,
    task_id: &str,
    test_command: Option<&str>,
    timeout: Duration,
) -> Result<PendingBaseTests, String> {
    let scratch_path = checkout_scratch(cwd, base_ref, task_id)?;
    let command = test_command.map(str::to_string);
    let handle = std::thread::spawn(move || run_tests(&scratch_path, command.as_deref(), timeout));
    Ok(PendingBaseTests { handle })
}

/// Two-sided verification: confirms the failure reproduces on the base
/// commit *and* the worktree is green after the fix (SWE-bench-style).
/// Whole-suite granularity, not per-test.
///
/// Takes both results rather than computing them, because the caller has
/// already run the post-fix suite as part of its retry loop — recomputing it
/// here re-ran the entire suite on an unchanged worktree for a guaranteed
/// identical answer.
pub fn combine_two_sided(post: TestResult, pre: TestResult) -> TwoSidedResult {
    TwoSidedResult {
        target_fixed: post.passed,
        no_regressions: post.passed,
        reproduced_on_base: Some(!pre.passed),
        regression_check_skipped: false,
        post,
        pre: Some(pre),
    }
}

pub fn combine_single_sided(post: TestResult) -> TwoSidedResult {
    TwoSidedResult {
        target_fixed: post.passed,
        no_regressions: post.passed,
        reproduced_on_base: None,
        regression_check_skipped: true,
        post,
        pre: None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::git_safety::{checkpoint, discard_worktree, ensure_worktree};
    use super::*;
    use std::fs;
    use std::process::Command as StdCommand;

    struct FailingRepo(std::path::PathBuf);
    impl FailingRepo {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("deepseek-axi-rust-verify-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("package.json"), r#"{"scripts":{"test":"node test.js"}}"#).unwrap();
            fs::write(
                path.join("test.js"),
                "const fs = require('fs');\nif (!fs.existsSync('fixed.flag')) { process.exit(1); }\nconsole.log('ok');\n",
            )
            .unwrap();
            let g = |args: &[&str]| StdCommand::new("git").args(args).current_dir(&path).output().unwrap();
            g(&["init", "-q"]);
            g(&["add", "-A"]);
            g(&[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "-m",
                "init (failing)",
            ]);
            Self(path)
        }
    }
    impl Drop for FailingRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn detect_test_command_finds_npm_test_for_a_real_script() {
        let repo = FailingRepo::new("detect");
        assert_eq!(detect_test_command(&repo.0), Some("npm test".to_string()));
    }

    #[test]
    fn detect_test_command_returns_none_for_the_npm_placeholder() {
        let dir = std::env::temp_dir().join(format!("deepseek-axi-rust-verify-notest-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.json"),
            format!(r#"{{"scripts":{{"test":"{NO_TEST_SCRIPT}"}}}}"#),
        )
        .unwrap();
        assert_eq!(detect_test_command(&dir), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_two_sided_reproduces_on_base_and_confirms_green_after_fix() {
        let repo = FailingRepo::new("twosided1");
        let taskid = "verify-two-sided-1";
        let info = ensure_worktree(&repo.0, taskid).unwrap();
        fs::write(info.path.join("fixed.flag"), "").unwrap();
        checkpoint(&info.path, "fix: add fixed.flag").unwrap();

        let post = run_tests(&info.path, Some("npm test"), Duration::from_secs(120));
        let pre = run_base_tests(
            &repo.0,
            &info.base_ref,
            taskid,
            Some("npm test"),
            Duration::from_secs(120),
        );
        let result = combine_two_sided(post, pre);

        assert_eq!(result.reproduced_on_base, Some(true));
        assert!(result.target_fixed);
        assert!(result.no_regressions);
        assert!(!result.regression_check_skipped);
        discard_worktree(&repo.0, taskid);
    }

    #[test]
    fn verify_two_sided_reports_target_fixed_false_when_still_red() {
        let repo = FailingRepo::new("twosided2");
        let taskid = "verify-two-sided-2";
        let info = ensure_worktree(&repo.0, taskid).unwrap();

        let post = run_tests(&info.path, Some("npm test"), Duration::from_secs(120));
        let pre = run_base_tests(
            &repo.0,
            &info.base_ref,
            taskid,
            Some("npm test"),
            Duration::from_secs(120),
        );
        let result = combine_two_sided(post, pre);

        assert_eq!(result.reproduced_on_base, Some(true));
        assert!(!result.target_fixed);
        discard_worktree(&repo.0, taskid);
    }

    #[test]
    fn verify_single_sided_only_uses_post_and_marks_skipped() {
        let repo = FailingRepo::new("singlesided");
        let taskid = "verify-single-sided-1";
        let info = ensure_worktree(&repo.0, taskid).unwrap();
        fs::write(info.path.join("fixed.flag"), "").unwrap();
        checkpoint(&info.path, "fix").unwrap();

        let result = combine_single_sided(run_tests(&info.path, Some("npm test"), Duration::from_secs(120)));
        assert!(result.target_fixed);
        assert!(result.regression_check_skipped);
        assert!(result.pre.is_none());
        discard_worktree(&repo.0, taskid);
    }

    #[test]
    fn spawned_base_tests_match_the_sequential_result() {
        // Guards the --parallel-verify path: backgrounding the base-commit run
        // must produce exactly the answer the sequential path would.
        let repo = FailingRepo::new("parallelbase");
        let taskid = "verify-parallel-base-1";
        let info = ensure_worktree(&repo.0, taskid).unwrap();

        let sequential = run_base_tests(
            &repo.0,
            &info.base_ref,
            taskid,
            Some("npm test"),
            Duration::from_secs(120),
        );
        let pending = spawn_base_tests(
            &repo.0,
            &info.base_ref,
            taskid,
            Some("npm test"),
            Duration::from_secs(120),
        )
        .unwrap();
        let parallel = pending.join(&repo.0, taskid);

        assert_eq!(sequential.ran, parallel.ran);
        assert_eq!(sequential.passed, parallel.passed);
        assert!(!parallel.passed, "base commit is the failing one");
        discard_worktree(&repo.0, taskid);
    }

    #[test]
    fn a_hanging_test_command_is_killed_at_its_deadline() {
        // The bug: `.output()` blocks until the child exits, and the run
        // budget is only checked between model turns — so a test command that
        // never returns hung the entire run forever, whatever --timeout said.
        let dir = std::env::temp_dir().join(format!("deepseek-axi-hang-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let started = Instant::now();
        let result = run_tests(&dir, Some("sleep 120"), Duration::from_secs(2));
        let elapsed = started.elapsed();

        assert!(result.ran);
        assert!(result.timed_out, "must report the deadline, not a plain failure");
        assert!(!result.passed, "a killed run is never a pass");
        assert!(result.output.contains("exceeded its 2s deadline"));
        assert!(
            elapsed < Duration::from_secs(30),
            "should return at the deadline, took {elapsed:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_deadline_reaches_grandchildren_not_just_the_shell() {
        // Killing only `sh` orphans what it spawned; those keep running and
        // hold the pipes open. The child leads its own process group so the
        // kill reaches the whole tree.
        let dir = std::env::temp_dir().join(format!("deepseek-axi-hang-tree-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let started = Instant::now();
        // `sh -c` spawns a subshell that sleeps; the outer shell waits on it.
        let result = run_tests(&dir, Some("sh -c 'sleep 120' & wait"), Duration::from_secs(2));
        assert!(result.timed_out);
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the grandchild kept the pipe open past the deadline"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_chatty_suite_does_not_deadlock_on_a_full_pipe_buffer() {
        // Polling try_wait() without draining stdout deadlocks once the OS
        // pipe buffer (~64KB) fills: the child blocks writing, so it never
        // exits, so try_wait never reports it done.
        let dir = std::env::temp_dir().join(format!("deepseek-axi-chatty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let started = Instant::now();
        // ~2MB of output, far past any pipe buffer.
        let result = run_tests(
            &dir,
            Some("yes deepseek | head -c 2000000; exit 0"),
            Duration::from_secs(60),
        );
        assert!(result.passed, "should exit cleanly, not stall: {}", result.timed_out);
        assert!(!result.timed_out);
        assert!(
            result.output.len() > 1_000_000,
            "output was truncated by a stalled pipe"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "deadlocked on a full pipe buffer"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_test_command_covers_non_npm_ecosystems() {
        // This very repo is Rust: npm-only detection meant `test --fix` could
        // not find a test command for its own project.
        let dir = std::env::temp_dir().join(format!("deepseek-axi-detect-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        assert_eq!(detect_test_command(&dir), None);

        fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        assert_eq!(detect_test_command(&dir), Some("cargo test".to_string()));
        fs::remove_file(dir.join("Cargo.toml")).unwrap();

        fs::write(dir.join("pyproject.toml"), "[project]\n").unwrap();
        assert_eq!(detect_test_command(&dir), Some("pytest".to_string()));
        fs::remove_file(dir.join("pyproject.toml")).unwrap();

        fs::write(dir.join("go.mod"), "module x\n").unwrap();
        assert_eq!(detect_test_command(&dir), Some("go test ./...".to_string()));

        let _ = fs::remove_dir_all(&dir);
    }
}
