#![allow(dead_code)] // GitContext.diff_stat/.error: debug/diagnostic fields, not read by every caller

use crate::ripgrep::search_text;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Default)]
pub struct GitContext {
    pub branch: Option<String>,
    pub touched_files: Vec<String>,
    pub diff: String,
    pub diff_stat: String,
    pub error: Option<String>,
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Never fails the caller — a non-git directory just yields an empty
/// context (mirrors `context-engine.js`'s `gitContext`).
pub fn git_context(cwd: &Path) -> GitContext {
    match run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(branch) => {
            let status_raw = run_git(cwd, &["status", "--porcelain"]).unwrap_or_default();
            let touched_files: Vec<String> = status_raw
                .lines()
                .filter(|l| l.len() > 3)
                .map(|l| l[3..].to_string())
                .collect();
            let diff = run_git(cwd, &["diff"]).unwrap_or_default();
            let diff_stat = run_git(cwd, &["diff", "--stat"]).unwrap_or_default();
            GitContext {
                branch: Some(branch.trim().to_string()),
                touched_files,
                diff,
                diff_stat,
                error: None,
            }
        }
        Err(e) => GitContext {
            error: Some(e),
            ..Default::default()
        },
    }
}

fn escape_regex(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if ".*+?^${}()|[]\\".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Finds files that reference a touched file's name — a symbol-name grep,
/// not real AST/import resolution (deliberately simple, §9 of
/// ENHANCEMENT_PLAN.md; tool-calling is the escape hatch when this misses
/// something).
pub fn discover_relevant_files(touched_files: &[String], cwd: &Path, max_results: usize) -> Vec<String> {
    let mut related: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for file in touched_files {
        if related.len() >= max_results {
            break;
        }
        let stem = Path::new(file).file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem.is_empty() {
            continue;
        }
        let pattern = format!(r"\b{}\b", escape_regex(stem));
        for m in search_text(&pattern, cwd, 10) {
            if related.len() >= max_results {
                break;
            }
            if let Some(path) = m.split(':').next()
                && !touched_files.iter().any(|t| t == path)
                && seen.insert(path.to_string())
            {
                related.push(path.to_string());
            }
        }
    }

    related
}

const TEST_DIR_HINTS: [&str; 3] = ["test", "tests", "__tests__"];

/// Convention-based test discovery (not exhaustive by design, §9): checks a
/// fixed set of plausible test paths and keeps only the ones that exist.
pub fn discover_tests(touched_files: &[String], cwd: &Path) -> Vec<String> {
    let mut candidates: BTreeSet<String> = BTreeSet::new();

    for file in touched_files {
        let path = Path::new(file);
        let dir = path.parent().unwrap_or(Path::new(""));
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem.is_empty() {
            continue;
        }
        let ext_suffix = if ext.is_empty() {
            String::new()
        } else {
            format!(".{ext}")
        };

        candidates.insert(
            dir.join(format!("{stem}.test{ext_suffix}"))
                .to_string_lossy()
                .into_owned(),
        );
        candidates.insert(
            dir.join(format!("{stem}.spec{ext_suffix}"))
                .to_string_lossy()
                .into_owned(),
        );
        for hint in TEST_DIR_HINTS {
            candidates.insert(
                dir.join(hint)
                    .join(format!("{stem}.test{ext_suffix}"))
                    .to_string_lossy()
                    .into_owned(),
            );
            candidates.insert(
                PathBuf::from(hint)
                    .join(format!("{stem}.test{ext_suffix}"))
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }

    candidates.into_iter().filter(|c| cwd.join(c).exists()).collect()
}

pub struct DynamicPart {
    pub label: String,
    pub priority: u32,
    pub text: String,
}

pub struct AssembledContext {
    pub prompt: String,
    pub tokens_estimate: usize,
    pub included: Vec<String>,
    pub dropped: Vec<String>,
}

fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Orders stable content first (the cache-hit-eligible prefix, §9), then
/// fills the remaining budget with dynamic parts in priority order (lower
/// number = kept first), dropping whatever doesn't fit. Never truncates the
/// stable prefix.
pub fn assemble_context(budget: usize, stable: &[String], dynamic: Vec<DynamicPart>) -> AssembledContext {
    let stable_text = stable
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut remaining = budget.saturating_sub(estimate_tokens(&stable_text));

    let mut ordered = dynamic;
    ordered.sort_by_key(|p| p.priority);

    let mut included_parts = Vec::new();
    let mut included_labels = Vec::new();
    let mut dropped = Vec::new();

    for part in ordered {
        let tokens = estimate_tokens(&part.text);
        if tokens <= remaining {
            remaining -= tokens;
            included_labels.push(part.label.clone());
            included_parts.push(part.text);
        } else {
            dropped.push(part.label);
        }
    }

    let mut all_parts = Vec::new();
    if !stable_text.is_empty() {
        all_parts.push(stable_text);
    }
    all_parts.extend(included_parts);
    let prompt = all_parts.join("\n\n");

    AssembledContext {
        tokens_estimate: estimate_tokens(&prompt),
        prompt,
        included: included_labels,
        dropped,
    }
}

#[derive(Serialize)]
pub struct BuiltContext {
    pub branch: Option<String>,
    #[serde(rename = "touchedFiles")]
    pub touched_files: Vec<String>,
    #[serde(rename = "relatedFiles")]
    pub related_files: Vec<String>,
    #[serde(rename = "testFiles")]
    pub test_files: Vec<String>,
    #[serde(rename = "tokensEstimate")]
    pub tokens_estimate: usize,
    pub included: Vec<String>,
    pub dropped: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

fn read_file_safe(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// The shared context-assembly pipeline used by both the standalone
/// `inspect` command and the worker loop.
pub fn build_context(
    task: &str,
    cwd: &Path,
    forced_files: &[String],
    budget: usize,
    max_files: usize,
) -> (BuiltContext, String) {
    let git = git_context(cwd);
    let mut touched_files: Vec<String> = git.touched_files.clone();
    for f in forced_files {
        if !touched_files.contains(f) {
            touched_files.push(f.clone());
        }
    }

    let related = discover_relevant_files(&touched_files, cwd, max_files);
    let tests = discover_tests(&touched_files, cwd);

    let file_block = |path: &str| -> String {
        match read_file_safe(&cwd.join(path)) {
            Some(content) => format!("--- {path} ---\n{content}"),
            None => String::new(),
        }
    };

    let mut dynamic = vec![DynamicPart {
        label: "git-diff".into(),
        priority: 0,
        text: if git.diff.is_empty() {
            String::new()
        } else {
            format!("--- git diff ---\n{}", git.diff)
        },
    }];
    for f in forced_files {
        dynamic.push(DynamicPart {
            label: format!("file:{f}"),
            priority: 1,
            text: file_block(f),
        });
    }
    for f in &related {
        dynamic.push(DynamicPart {
            label: format!("related:{f}"),
            priority: 2,
            text: file_block(f),
        });
    }
    for f in &tests {
        dynamic.push(DynamicPart {
            label: format!("test:{f}"),
            priority: 2,
            text: file_block(f),
        });
    }
    dynamic.retain(|p| !p.text.is_empty());

    let stable = vec![
        format!("Task: {task}"),
        format!("Branch: {}", git.branch.clone().unwrap_or_else(|| "unknown".into())),
    ];
    let assembled = assemble_context(budget, &stable, dynamic);

    let built = BuiltContext {
        branch: git.branch,
        touched_files,
        related_files: related,
        test_files: tests,
        tokens_estimate: assembled.tokens_estimate,
        included: assembled.included,
        dropped: assembled.dropped,
        prompt: None,
    };
    (built, assembled.prompt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command as StdCommand;

    // Nanosecond-in-current-second timestamps can collide under parallel
    // test execution (cargo test runs tests concurrently by default) — an
    // atomic counter is deterministically collision-free within a process
    // regardless of clock resolution. This, plus checking git command exit
    // status instead of only that the process spawned, fixed a real
    // intermittent flake in this test.
    static TEMP_REPO_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    struct TempRepo(PathBuf);
    impl TempRepo {
        fn new() -> Self {
            let n = TEMP_REPO_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!("deepseek-axi-rust-ctx-{}-{n}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            let git = |args: &[&str]| {
                let output = StdCommand::new("git").args(args).current_dir(&path).output().unwrap();
                assert!(
                    output.status.success(),
                    "git {args:?} failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            };
            git(&["init", "-q"]);
            fs::write(path.join("utils.js"), "export function helper() {\n  return 1;\n}\n").unwrap();
            git(&["add", "-A"]);
            git(&[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "-m",
                "init",
            ]);
            Self(path)
        }
    }
    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn git_context_reports_branch_status_and_diff_for_a_real_repo() {
        let repo = TempRepo::new();
        fs::write(
            repo.0.join("utils.js"),
            "export function helper() {\n  return 1;\n}\n// a change\n",
        )
        .unwrap();
        let ctx = git_context(&repo.0);
        assert!(ctx.branch.is_some());
        assert_eq!(ctx.touched_files, vec!["utils.js".to_string()]);
        assert!(ctx.diff.contains("a change"));
    }

    #[test]
    fn git_context_returns_empty_non_throwing_context_outside_a_git_repo() {
        let dir = std::env::temp_dir().join(format!("deepseek-axi-rust-nogit-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let ctx = git_context(&dir);
        assert!(ctx.branch.is_none());
        assert!(ctx.touched_files.is_empty());
        assert!(ctx.error.is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_relevant_files_finds_a_file_that_references_a_touched_files_name() {
        let repo = TempRepo::new();
        fs::write(
            repo.0.join("index.js"),
            "import { helper } from './utils.js';\nhelper();\n",
        )
        .unwrap();
        let related = discover_relevant_files(&["utils.js".to_string()], &repo.0, 20);
        assert!(
            related.contains(&"index.js".to_string()),
            "expected index.js in {related:?}"
        );
    }

    #[test]
    fn discover_tests_only_returns_files_that_exist() {
        let repo = TempRepo::new();
        fs::create_dir_all(repo.0.join("test")).unwrap();
        fs::write(repo.0.join("test").join("utils.test.js"), "// exists\n").unwrap();
        let tests = discover_tests(&["utils.js".to_string()], &repo.0);
        assert_eq!(tests, vec!["test/utils.test.js".to_string()]);
    }

    #[test]
    fn assemble_context_keeps_stable_prefix_and_orders_by_priority() {
        let result = assemble_context(
            1000,
            &["stable header".to_string()],
            vec![
                DynamicPart {
                    label: "low".into(),
                    priority: 5,
                    text: "low priority content".into(),
                },
                DynamicPart {
                    label: "high".into(),
                    priority: 0,
                    text: "high priority content".into(),
                },
            ],
        );
        assert!(result.prompt.starts_with("stable header"));
        assert!(result.prompt.find("high priority content") < result.prompt.find("low priority content"));
        assert!(result.dropped.is_empty());
    }

    #[test]
    fn assemble_context_drops_lowest_priority_first_never_the_stable_prefix() {
        let stable = vec!["x".repeat(40)];
        let result = assemble_context(
            15,
            &stable,
            vec![
                DynamicPart {
                    label: "keep".into(),
                    priority: 0,
                    text: "y".repeat(8),
                },
                DynamicPart {
                    label: "drop".into(),
                    priority: 1,
                    text: "z".repeat(400),
                },
            ],
        );
        assert!(result.prompt.starts_with('x'));
        assert!(result.included.contains(&"keep".to_string()));
        assert!(result.dropped.contains(&"drop".to_string()));
    }
}
