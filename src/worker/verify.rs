#![allow(dead_code)] // TwoSidedResult.pre: kept for diagnostics/handoff detail, not read by every caller

use super::git_safety::{checkout_scratch, remove_scratch};
use std::path::Path;
use std::process::Command;

const NO_TEST_SCRIPT: &str = "echo \"Error: no test specified\" && exit 1";

pub fn detect_test_command(cwd: &Path) -> Option<String> {
    let text = std::fs::read_to_string(cwd.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let test_script = json.get("scripts")?.get("test")?.as_str()?;
    if test_script == NO_TEST_SCRIPT {
        return None;
    }
    Some("npm test".to_string())
}

pub struct TestResult {
    pub ran: bool,
    pub passed: bool,
    pub output: String,
}

/// Runs the test command once, classifying pass/fail at whole-suite
/// granularity. Raw failure output is returned as-is for the model to
/// diagnose — deliberately not parsed per test framework.
pub fn run_tests(cwd: &Path, test_command: Option<&str>) -> TestResult {
    let command = test_command.map(str::to_string).or_else(|| detect_test_command(cwd));
    let Some(command) = command else {
        return TestResult {
            ran: false,
            passed: false,
            output: "No test command detected or provided".into(),
        };
    };

    match Command::new("sh").arg("-c").arg(&command).current_dir(cwd).output() {
        Ok(out) => TestResult {
            ran: true,
            passed: out.status.success(),
            output: format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        },
        Err(e) => TestResult {
            ran: true,
            passed: false,
            output: e.to_string(),
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

/// Two-sided verification: confirms the failure reproduces on the base
/// commit *and* the worktree is green after the fix (SWE-bench-style).
/// Whole-suite granularity, not per-test — see module docs in the Node
/// build's `verify.js` for why (§18 of ENHANCEMENT_PLAN.md).
pub fn verify_two_sided(
    cwd: &Path,
    worktree_path: &Path,
    base_ref: &str,
    test_command: Option<&str>,
    task_id: &str,
) -> TwoSidedResult {
    let post = run_tests(worktree_path, test_command);

    let pre = match checkout_scratch(cwd, base_ref, task_id) {
        Ok(scratch_path) => {
            let result = run_tests(&scratch_path, test_command);
            remove_scratch(cwd, task_id);
            result
        }
        Err(e) => TestResult {
            ran: false,
            passed: false,
            output: e,
        },
    };

    TwoSidedResult {
        target_fixed: post.passed,
        no_regressions: post.passed,
        reproduced_on_base: Some(!pre.passed),
        regression_check_skipped: false,
        post,
        pre: Some(pre),
    }
}

pub fn verify_single_sided(worktree_path: &Path, test_command: Option<&str>) -> TwoSidedResult {
    let post = run_tests(worktree_path, test_command);
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

        let result = verify_two_sided(&repo.0, &info.path, &info.base_ref, Some("npm test"), taskid);
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
        let result = verify_two_sided(&repo.0, &info.path, &info.base_ref, Some("npm test"), taskid);
        assert_eq!(result.reproduced_on_base, Some(true));
        assert!(!result.target_fixed);
        discard_worktree(&repo.0, taskid);
    }

    #[test]
    fn verify_single_sided_only_runs_post_and_marks_skipped() {
        let repo = FailingRepo::new("singlesided");
        let taskid = "verify-single-sided-1";
        let info = ensure_worktree(&repo.0, taskid).unwrap();
        fs::write(info.path.join("fixed.flag"), "").unwrap();
        checkpoint(&info.path, "fix").unwrap();

        let result = verify_single_sided(&info.path, Some("npm test"));
        assert!(result.target_fixed);
        assert!(result.regression_check_skipped);
        assert!(result.pre.is_none());
        discard_worktree(&repo.0, taskid);
    }
}
