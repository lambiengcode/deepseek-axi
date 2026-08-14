#![allow(dead_code)] // DiffSummary.diff/.truncated: full diff text/truncation flag, not read by every caller (loop.js only needs files_changed today)

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn worktree_root() -> PathBuf {
    std::env::temp_dir().join("deepseek-axi-worktrees")
}

pub fn branch_name(task_id: &str) -> String {
    format!("deepseek-axi/{task_id}")
}

pub fn worktree_path_for(task_id: &str) -> PathBuf {
    worktree_root().join(task_id)
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn current_head(cwd: &Path) -> Result<String, String> {
    git(cwd, &["rev-parse", "HEAD"])
}

pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: String,
    pub base_ref: String,
}

/// Creates (or reuses) an isolated worktree on its own branch off current
/// HEAD — the safety boundary: every worker-loop edit happens here, never
/// on the caller's real working tree or branch.
pub fn ensure_worktree(cwd: &Path, task_id: &str) -> Result<WorktreeInfo, String> {
    let root = worktree_root();
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let path = worktree_path_for(task_id);
    let branch = branch_name(task_id);
    let base_ref = current_head(cwd)?;
    if !path.exists() {
        git(
            cwd,
            &["worktree", "add", "-b", &branch, path.to_str().unwrap_or_default()],
        )?;
    }
    Ok(WorktreeInfo { path, branch, base_ref })
}

/// Commits whatever changed since the last checkpoint. Returns `None` when
/// there was nothing to commit.
pub fn checkpoint(worktree_path: &Path, message: &str) -> Result<Option<String>, String> {
    git(worktree_path, &["add", "-A"])?;
    let status = git(worktree_path, &["status", "--porcelain"])?;
    if status.trim().is_empty() {
        return Ok(None);
    }
    git(
        worktree_path,
        &[
            "-c",
            "user.name=deepseek-axi",
            "-c",
            "user.email=deepseek-axi@localhost",
            "commit",
            "-q",
            "-m",
            message,
        ],
    )?;
    Ok(Some(git(worktree_path, &["rev-parse", "HEAD"])?))
}

pub struct FileChangedRow {
    pub path: String,
    pub insertions: u32,
    pub deletions: u32,
}

pub struct DiffSummary {
    pub diff: String,
    pub truncated: bool,
    pub files_changed: Vec<FileChangedRow>,
}

const DIFF_CHAR_LIMIT: usize = 20_000;

pub fn diff_summary(worktree_path: &Path, base_ref: &str) -> Result<DiffSummary, String> {
    let numstat = git(worktree_path, &["diff", "--numstat", base_ref, "HEAD"])?;
    let diff_full = git(worktree_path, &["diff", base_ref, "HEAD"])?;
    let truncated = diff_full.chars().count() > DIFF_CHAR_LIMIT;
    let diff = if truncated {
        diff_full.chars().take(DIFF_CHAR_LIMIT).collect()
    } else {
        diff_full
    };

    let files_changed = numstat
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let ins = parts.next()?;
            let del = parts.next()?;
            let path = parts.next()?;
            Some(FileChangedRow {
                path: path.to_string(),
                insertions: ins.parse().unwrap_or(0),
                deletions: del.parse().unwrap_or(0),
            })
        })
        .collect();

    Ok(DiffSummary {
        diff,
        truncated,
        files_changed,
    })
}

/// Removes the worktree and its branch entirely — used on failure/dry-run.
pub fn discard_worktree(cwd: &Path, task_id: &str) {
    let path = worktree_path_for(task_id);
    let _ = git(
        cwd,
        &["worktree", "remove", "--force", path.to_str().unwrap_or_default()],
    );
    let _ = git(cwd, &["branch", "-D", &branch_name(task_id)]);
}

/// Removes the worktree checkout but keeps the branch — deepseek-axi never
/// merges into the caller's branch itself.
pub fn promote(cwd: &Path, task_id: &str) -> Result<String, String> {
    let path = worktree_path_for(task_id);
    git(
        cwd,
        &["worktree", "remove", "--force", path.to_str().unwrap_or_default()],
    )?;
    Ok(branch_name(task_id))
}

/// A short-lived detached worktree at an arbitrary ref, for the pre-fix
/// test run in `verify.rs`.
pub fn checkout_scratch(cwd: &Path, git_ref: &str, task_id: &str) -> Result<PathBuf, String> {
    let path = worktree_root().join(format!("{task_id}-scratch"));
    if path.exists() {
        let _ = git(
            cwd,
            &["worktree", "remove", "--force", path.to_str().unwrap_or_default()],
        );
    }
    git(
        cwd,
        &[
            "worktree",
            "add",
            "--detach",
            "-q",
            path.to_str().unwrap_or_default(),
            git_ref,
        ],
    )?;
    Ok(path)
}

pub fn remove_scratch(cwd: &Path, task_id: &str) {
    let path = worktree_root().join(format!("{task_id}-scratch"));
    let _ = git(
        cwd,
        &["worktree", "remove", "--force", path.to_str().unwrap_or_default()],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::write;

    struct TempRepo(PathBuf);
    impl TempRepo {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("deepseek-axi-rust-safety-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            let g = |args: &[&str]| Command::new("git").args(args).current_dir(&path).output().unwrap();
            g(&["init", "-q"]);
            write(path.join("a.txt"), "one\n").unwrap();
            g(&["add", "-A"]);
            g(&[
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
    fn ensure_worktree_creates_isolated_worktree_on_its_own_branch() {
        let repo = TempRepo::new("ensure");
        let taskid = "gs-ensure-1";
        let info = ensure_worktree(&repo.0, taskid).unwrap();
        assert!(info.path.exists());
        assert_eq!(info.branch, branch_name(taskid));
        assert_eq!(info.base_ref, current_head(&repo.0).unwrap());
        assert_eq!(current_head(&repo.0).unwrap(), info.base_ref);
        discard_worktree(&repo.0, taskid);
    }

    #[test]
    fn checkpoint_commits_and_returns_none_when_nothing_changed() {
        let repo = TempRepo::new("checkpoint");
        let taskid = "gs-checkpoint-1";
        let info = ensure_worktree(&repo.0, taskid).unwrap();
        assert_eq!(checkpoint(&info.path, "noop").unwrap(), None);
        write(info.path.join("a.txt"), "one\ntwo\n").unwrap();
        let sha = checkpoint(&info.path, "add a line").unwrap();
        assert!(sha.is_some());
        assert_eq!(sha.unwrap(), current_head(&info.path).unwrap());
        discard_worktree(&repo.0, taskid);
    }

    #[test]
    fn diff_summary_reports_accurate_files_changed() {
        let repo = TempRepo::new("diff");
        let taskid = "gs-diff-1";
        let info = ensure_worktree(&repo.0, taskid).unwrap();
        write(info.path.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        checkpoint(&info.path, "expand").unwrap();
        let summary = diff_summary(&info.path, &info.base_ref).unwrap();
        assert_eq!(summary.files_changed.len(), 1);
        assert_eq!(summary.files_changed[0].path, "a.txt");
        assert_eq!(summary.files_changed[0].insertions, 2);
        assert_eq!(summary.files_changed[0].deletions, 0);
        assert!(summary.diff.contains("+two"));
        discard_worktree(&repo.0, taskid);
    }

    #[test]
    fn discard_worktree_removes_directory_and_branch() {
        let repo = TempRepo::new("discard");
        let taskid = "gs-discard-1";
        let info = ensure_worktree(&repo.0, taskid).unwrap();
        discard_worktree(&repo.0, taskid);
        assert!(!info.path.exists());
        let branches = git(&repo.0, &["branch", "--list", &branch_name(taskid)]).unwrap();
        assert!(branches.trim().is_empty());
    }

    #[test]
    fn promote_removes_worktree_but_keeps_branch() {
        let repo = TempRepo::new("promote");
        let taskid = "gs-promote-1";
        let info = ensure_worktree(&repo.0, taskid).unwrap();
        write(info.path.join("a.txt"), "one\ntwo\n").unwrap();
        checkpoint(&info.path, "change").unwrap();
        promote(&repo.0, taskid).unwrap();
        assert!(!info.path.exists());
        let branches = git(&repo.0, &["branch", "--list", &branch_name(taskid)]).unwrap();
        assert!(branches.contains(&branch_name(taskid)));
        let _ = git(&repo.0, &["branch", "-D", &branch_name(taskid)]);
    }

    #[test]
    fn checkout_scratch_and_remove_scratch_work() {
        let repo = TempRepo::new("scratch");
        let taskid = "gs-scratch-1";
        let base_ref = current_head(&repo.0).unwrap();
        let scratch_path = checkout_scratch(&repo.0, &base_ref, taskid).unwrap();
        assert!(scratch_path.exists());
        assert_eq!(current_head(&scratch_path).unwrap(), base_ref);
        remove_scratch(&repo.0, taskid);
        assert!(!scratch_path.exists());
    }
}
