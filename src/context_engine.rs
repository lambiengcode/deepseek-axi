#![allow(dead_code)] // GitContext.error: diagnostic field, not read by every caller

use crate::ripgrep::search_files;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Default)]
pub struct GitContext {
    pub branch: Option<String>,
    pub touched_files: Vec<String>,
    pub diff: String,
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
            let touched_files: Vec<String> = status_raw.lines().filter_map(porcelain_path).collect();
            let diff = run_git(cwd, &["diff"]).unwrap_or_default();
            GitContext {
                branch: Some(branch.trim().to_string()),
                touched_files,
                diff,
                error: None,
            }
        }
        Err(e) => GitContext {
            error: Some(e),
            ..Default::default()
        },
    }
}

/// Extracts the path from one `git status --porcelain` line.
///
/// Renames are emitted as `R  old.rs -> new.rs`; naively taking everything
/// after the status columns yields the literal string `old.rs -> new.rs`,
/// whose `file_stem()` is `"old.rs -> new"` — a pattern that matches nothing,
/// so a renamed file cost a full repo scan and returned zero related files.
fn porcelain_path(line: &str) -> Option<String> {
    if line.len() <= 3 {
        return None;
    }
    let rest = line[3..].trim();
    let path = match rest.split_once(" -> ") {
        Some((_old, new)) => new,
        None => rest,
    };
    // Paths with unusual characters are quoted by git (core.quotePath).
    let path = path.trim_matches('"');
    if path.is_empty() { None } else { Some(path.to_string()) }
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
    // One alternation regex over one walk, instead of a full repo scan per
    // touched file. With 10 modified files the old shape read and scanned
    // every file in the repo 10 times before the first API call.
    let stems: Vec<String> = touched_files
        .iter()
        .filter_map(|f| Path::new(f).file_stem().and_then(|s| s.to_str()))
        .filter(|s| !s.is_empty())
        .map(escape_regex)
        .collect::<BTreeSet<_>>() // dedupe; also gives a deterministic order
        .into_iter()
        .collect();

    if stems.is_empty() {
        return Vec::new();
    }

    let pattern = format!(r"\b({})\b", stems.join("|"));
    // Ask for extra headroom because touched files themselves match the
    // pattern and are filtered out below.
    let budget = max_results.saturating_add(touched_files.len());

    let mut related: Vec<String> = search_files(&pattern, cwd, budget)
        .into_iter()
        .filter(|path| !touched_files.iter().any(|t| t == path))
        .take(max_results)
        .collect();

    // Deterministic order: this list feeds the prompt, which is a cache prefix.
    related.sort();
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
    /// Drop order when the budget is tight: lower is kept first.
    pub priority: u32,
    /// Emit order: lower is more likely to be byte-identical on the next
    /// run, so it belongs nearer the front of the cache prefix.
    pub stability: u32,
    pub text: String,
}

pub struct AssembledContext {
    pub prompt: String,
    pub tokens_estimate: usize,
    pub included: Vec<String>,
    pub dropped: Vec<String>,
}

/// Deliberately a slight *over*-estimate. `len()` is bytes, and source code
/// with heavy punctuation and indentation runs closer to 3 bytes/token than
/// 4, so the old `/4` undershot by ~20% and quietly shipped more than the
/// caller's budget. Erring high costs a little context; erring low costs
/// money and can overflow the window.
fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(3)
}

/// Reserved by the worker loop out of its context budget for things the
/// assembled prompt doesn't contain but the request still carries: the
/// ~540-token tool schemas, the system prompt, and the phase instruction
/// appended after the context. Applied by the caller rather than here, since
/// `inspect` sends none of those.
pub const BUDGET_RESERVE_TOKENS: usize = 900;

/// Cuts a part down to a token budget on a line boundary, so a too-large
/// part contributes what fits instead of contributing nothing.
fn truncate_to_tokens(text: &str, max_tokens: usize) -> Option<String> {
    if max_tokens == 0 {
        return None;
    }
    let max_bytes = max_tokens.saturating_mul(3);
    if text.len() <= max_bytes {
        return Some(text.to_string());
    }
    let mut out = String::with_capacity(max_bytes);
    for line in text.lines() {
        // +1 for the newline we are about to add.
        if out.len() + line.len() + 1 > max_bytes {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    if out.is_empty() {
        return None;
    }
    out.push_str("… [truncated to fit the context budget]\n");
    Some(out)
}

/// Selects dynamic parts by `priority` (lower = kept first when the budget
/// is tight), then emits them by `stability` (lower = more likely to be
/// byte-identical next run, so it belongs nearer the front of the prompt).
///
/// Those two orders are deliberately separate. DeepSeek's cache only hits on
/// a *fully matching prefix*, so the most-reusable bytes must come first —
/// but the most-reusable content is not the most important content. `pinned`
/// text is never dropped and is emitted **last**, because it is the
/// per-run-unique part (the task, the branch) that would otherwise poison
/// the prefix at byte 0.
pub fn assemble_context(budget: usize, pinned: &[String], dynamic: Vec<DynamicPart>) -> AssembledContext {
    let pinned_text = pinned
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut remaining = budget.saturating_sub(estimate_tokens(&pinned_text));

    let mut ordered = dynamic;
    ordered.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.label.cmp(&b.label)));

    let mut kept: Vec<DynamicPart> = Vec::new();
    let mut included_labels = Vec::new();
    let mut dropped = Vec::new();

    for part in ordered {
        let tokens = estimate_tokens(&part.text);
        if tokens <= remaining {
            remaining -= tokens;
            included_labels.push(part.label.clone());
            kept.push(part);
        } else if let Some(clipped) = truncate_to_tokens(&part.text, remaining) {
            // Partial context beats none: a large `git diff` used to yield
            // zero diff rather than the first N lines of one.
            remaining = remaining.saturating_sub(estimate_tokens(&clipped));
            included_labels.push(format!("{} (truncated)", part.label));
            kept.push(DynamicPart {
                label: part.label,
                priority: part.priority,
                stability: part.stability,
                text: clipped,
            });
        } else {
            dropped.push(part.label);
        }
    }

    kept.sort_by(|a, b| a.stability.cmp(&b.stability).then_with(|| a.label.cmp(&b.label)));

    let mut all_parts: Vec<String> = kept.into_iter().map(|p| p.text).collect();
    if !pinned_text.is_empty() {
        all_parts.push(pinned_text);
    }
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

/// Generated/vendored files that are large, uninformative, and would eat the
/// whole budget if a content grep happened to match inside them.
const CONTEXT_DENYLIST_SUFFIXES: [&str; 7] = [
    ".min.js",
    ".min.css",
    ".map",
    "-lock.json",
    ".lock",
    ".snap",
    ".bundle.js",
];

/// Files above this size are never inlined. They would either consume the
/// entire budget or be dropped after being read — either way the read itself
/// is wasted work.
const MAX_INLINE_FILE_BYTES: u64 = 64 * 1024;

fn is_denylisted(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    CONTEXT_DENYLIST_SUFFIXES.iter().any(|s| lower.ends_with(s))
}

/// Checks size *before* reading, so a 10MB bundle costs a `stat` rather than
/// a 10MB allocation that then gets discarded for being too big.
fn read_file_safe(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_INLINE_FILE_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Files that carry a project's standing instructions to a coding agent, in
/// precedence order within a directory. `AGENTS.md` is the cross-vendor
/// convention; `CLAUDE.md` is the widely-deployed older spelling.
const INSTRUCTION_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// Nearest ancestor containing `.git`, else `cwd`. Matches how every other
/// coding agent scopes a project. Note `.git` is a *file* inside a worktree,
/// not a directory, so this resolves the worktree root correctly.
fn project_root(cwd: &Path) -> PathBuf {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        current = dir.parent();
    }
    cwd.to_path_buf()
}

/// Collects the project's agent instructions, walking from the project root
/// down to `cwd` so the most specific file comes last.
///
/// These files are how a repository tells an agent its conventions, and every
/// other coding agent reads them. Without this the worker tier would happily
/// violate rules the user had already written down.
///
/// Byte-identical duplicates collapse to their first occurrence, so a
/// `CLAUDE.md` that merely repeats its sibling `AGENTS.md` is not paid for
/// twice.
pub fn discover_instructions(cwd: &Path) -> Vec<(String, String)> {
    let root = project_root(cwd);

    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut current = Some(cwd);
    while let Some(dir) = current {
        dirs.push(dir.to_path_buf());
        if dir == root {
            break;
        }
        current = dir.parent();
    }
    dirs.reverse();

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut found = Vec::new();
    for dir in dirs {
        for name in INSTRUCTION_FILES {
            let path = dir.join(name);
            let Some(content) = read_file_safe(&path) else {
                continue;
            };
            let trimmed = content.trim().to_string();
            if trimmed.is_empty() || !seen.insert(trimmed) {
                continue;
            }
            let label = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().into_owned();
            found.push((label, content));
        }
    }
    found
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

    let related: Vec<String> = discover_relevant_files(&touched_files, cwd, max_files)
        .into_iter()
        .filter(|f| !is_denylisted(f))
        .collect();
    // A test for `utils.js` almost always contains the identifier `utils`, so
    // it is found by the content grep *and* by convention-based discovery.
    // Without this it was read twice and inlined into the prompt twice
    // verbatim, displacing real source files into `dropped`.
    let tests: Vec<String> = discover_tests(&touched_files, cwd)
        .into_iter()
        .filter(|t| !related.contains(t))
        .collect();

    let file_block = |path: &str| -> String {
        match read_file_safe(&cwd.join(path)) {
            Some(content) => format!("--- {path} ---\n{content}"),
            None => String::new(),
        }
    };

    // stability: instructions change least of all, then file contents, then
    // the diff; the task/branch (pinned, below) change every run.
    let mut dynamic: Vec<DynamicPart> = discover_instructions(cwd)
        .into_iter()
        .map(|(label, content)| DynamicPart {
            label: format!("instructions:{label}"),
            // Project conventions outrank discovered files under budget
            // pressure: violating a written rule is worse than missing a file.
            priority: 0,
            stability: 0,
            text: format!("--- project instructions ({label}) ---\n{content}"),
        })
        .collect();
    dynamic.push(DynamicPart {
        label: "git-diff".into(),
        priority: 0,
        stability: 10,
        text: if git.diff.is_empty() {
            String::new()
        } else {
            format!("--- git diff ---\n{}", git.diff)
        },
    });
    for f in forced_files {
        dynamic.push(DynamicPart {
            label: format!("file:{f}"),
            priority: 1,
            stability: 1,
            text: file_block(f),
        });
    }
    for f in &related {
        dynamic.push(DynamicPart {
            label: format!("related:{f}"),
            priority: 2,
            stability: 1,
            text: file_block(f),
        });
    }
    for f in &tests {
        dynamic.push(DynamicPart {
            label: format!("test:{f}"),
            priority: 2,
            stability: 1,
            text: file_block(f),
        });
    }
    dynamic.retain(|p| !p.text.is_empty());

    // Pinned: never dropped, emitted last. These are unique per run, so
    // putting them first (as this did) meant byte 0 of every request differed
    // and no cache prefix was ever shared across runs.
    let pinned = vec![
        format!("Task: {task}"),
        format!("Branch: {}", git.branch.clone().unwrap_or_else(|| "unknown".into())),
    ];
    let assembled = assemble_context(budget, &pinned, dynamic);

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
    fn assemble_context_emits_pinned_last_and_orders_dynamic_by_stability() {
        let result = assemble_context(
            1000,
            &["PINNED-TAIL".to_string()],
            vec![
                DynamicPart {
                    label: "volatile".into(),
                    priority: 0,
                    stability: 10,
                    text: "VOLATILE".into(),
                },
                DynamicPart {
                    label: "stable".into(),
                    priority: 5,
                    stability: 0,
                    text: "STABLE".into(),
                },
            ],
        );
        // Emit order follows stability, not priority: a cache hit needs the
        // least-changing bytes at the front, and priority is about what to
        // drop, which is a different question.
        assert!(result.prompt.find("STABLE") < result.prompt.find("VOLATILE"));
        // The pinned text is unique per run (task, branch). Emitting it first
        // meant byte 0 differed every run and no prefix was ever shared.
        assert!(result.prompt.ends_with("PINNED-TAIL"));
        assert!(result.dropped.is_empty());
    }

    #[test]
    fn assemble_context_selects_by_priority_when_the_budget_is_tight() {
        // pinned "PINNED" = 6 bytes = 2 tokens, leaving 18 of the 20 budget.
        let result = assemble_context(
            20,
            &["PINNED".to_string()],
            vec![
                DynamicPart {
                    label: "keep".into(),
                    priority: 0,
                    stability: 0,
                    text: "k".repeat(30), // 10 tokens, fits
                },
                DynamicPart {
                    label: "drop".into(),
                    priority: 1,
                    stability: 0,
                    // 34 tokens on a single line, so not even a partial line fits
                    text: "d".repeat(100),
                },
            ],
        );
        assert!(result.included.contains(&"keep".to_string()));
        assert!(result.dropped.contains(&"drop".to_string()));
        assert!(result.prompt.ends_with("PINNED"));
    }

    #[test]
    fn assemble_context_truncates_rather_than_dropping_when_some_lines_fit() {
        // A large `git diff` used to contribute nothing at all rather than
        // the part of it that fit.
        let big = (0..50).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let result = assemble_context(
            20,
            &[],
            vec![DynamicPart {
                label: "git-diff".into(),
                priority: 0,
                stability: 0,
                text: big,
            }],
        );
        assert!(result.dropped.is_empty());
        assert!(result.included.iter().any(|l| l == "git-diff (truncated)"));
        assert!(result.prompt.contains("line 0"));
        assert!(!result.prompt.contains("line 49"));
        assert!(result.prompt.contains("truncated to fit"));
    }

    #[test]
    fn instructions_are_collected_root_first_and_deduplicated() {
        let repo = TempRepo::new();
        // Root conventions, plus a nested directory that adds its own and
        // also carries a CLAUDE.md that merely duplicates its sibling.
        fs::write(repo.0.join("AGENTS.md"), "root rules\n").unwrap();
        let nested = repo.0.join("crates").join("inner");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("AGENTS.md"), "inner rules\n").unwrap();
        fs::write(nested.join("CLAUDE.md"), "  inner rules  \n").unwrap();

        let found = discover_instructions(&nested);
        let labels: Vec<&str> = found.iter().map(|(l, _)| l.as_str()).collect();

        // Root first, most specific last — precedence reads top to bottom.
        assert_eq!(labels.len(), 2, "the duplicate CLAUDE.md must collapse: {labels:?}");
        assert_eq!(labels[0], "AGENTS.md");
        assert!(labels[1].ends_with("AGENTS.md") && labels[1].contains("inner"));
        assert_eq!(found[0].1.trim(), "root rules");
        assert_eq!(found[1].1.trim(), "inner rules");
    }

    #[test]
    fn a_claude_md_that_differs_from_its_sibling_is_kept() {
        let repo = TempRepo::new();
        fs::write(repo.0.join("AGENTS.md"), "shared rules\n").unwrap();
        fs::write(repo.0.join("CLAUDE.md"), "claude-specific rules\n").unwrap();

        let found = discover_instructions(&repo.0);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, "AGENTS.md", "AGENTS.md takes precedence within a directory");
        assert_eq!(found[1].0, "CLAUDE.md");
    }

    #[test]
    fn instructions_are_absent_and_harmless_when_the_repo_has_none() {
        let repo = TempRepo::new();
        assert!(discover_instructions(&repo.0).is_empty());
    }

    #[test]
    fn build_context_puts_instructions_ahead_of_everything_else() {
        let repo = TempRepo::new();
        fs::write(repo.0.join("AGENTS.md"), "always run cargo fmt\n").unwrap();
        fs::write(repo.0.join("a.rs"), "fn a() {}\n").unwrap();

        let (built, prompt) = build_context("do a thing", &repo.0, &["a.rs".to_string()], 4000, 10);
        assert!(
            built.included.iter().any(|l| l == "instructions:AGENTS.md"),
            "included: {:?}",
            built.included
        );
        // Most stable content leads the prompt (cache prefix), task/branch last.
        assert!(prompt.contains("always run cargo fmt"));
        assert!(
            prompt.find("project instructions") < prompt.find("--- a.rs ---"),
            "instructions must precede file contents"
        );
        assert!(prompt.trim_end().ends_with("Branch: main") || prompt.contains("Task: do a thing"));
    }

    #[test]
    fn porcelain_path_takes_the_destination_of_a_rename() {
        // `R  old.rs -> new.rs` previously yielded the whole arrow string,
        // whose file_stem is "old.rs -> new" — a pattern matching nothing, so
        // a renamed file cost a full repo scan and returned no related files.
        assert_eq!(porcelain_path("R  old.rs -> new.rs"), Some("new.rs".to_string()));
        assert_eq!(porcelain_path(" M src/lib.rs"), Some("src/lib.rs".to_string()));
        assert_eq!(porcelain_path("?? a.txt"), Some("a.txt".to_string()));
        assert_eq!(
            porcelain_path("R  \"a b.rs\" -> \"c d.rs\""),
            Some("c d.rs".to_string())
        );
        assert_eq!(porcelain_path(""), None);
        assert_eq!(porcelain_path("M  "), None);
    }

    #[test]
    fn oversized_and_generated_files_are_not_inlined() {
        let dir = tempfile_dir_for("size-guard");
        let big = dir.join("big.txt");
        fs::write(&big, "x".repeat((MAX_INLINE_FILE_BYTES + 1) as usize)).unwrap();
        assert_eq!(read_file_safe(&big), None, "oversized file must not be read in");

        let small = dir.join("small.txt");
        fs::write(&small, "hello").unwrap();
        assert_eq!(read_file_safe(&small), Some("hello".to_string()));

        assert!(is_denylisted("dist/app.min.js"));
        assert!(is_denylisted("package-lock.json"));
        assert!(is_denylisted("Cargo.lock"));
        assert!(is_denylisted("__snapshots__/a.snap"));
        assert!(!is_denylisted("src/main.rs"));
        let _ = fs::remove_dir_all(&dir);
    }

    fn tempfile_dir_for(tag: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("deepseek-axi-ctx-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
