//! File-content search backed directly by ripgrep's own library crates
//! (`grep` + `ignore`) instead of shelling out to a separately-installed
//! `rg` binary or a hand-rolled walker — the concrete Rust-specific win
//! from RUST_MIGRATION_PLAN.md §1 (SIMD-accelerated transitively via
//! `memchr`/`regex` inside these crates, and `ignore` respects .gitignore
//! correctly instead of a hardcoded skip-list).

use grep::regex::RegexMatcher;
use grep::searcher::SearcherBuilder;
use grep::searcher::sinks::UTF8;
use ignore::WalkBuilder;
use std::path::Path;

// `ignore`'s .gitignore handling only activates inside a real git repo
// (`require_git: true` by default, confirmed against ignore-0.4.33's
// source) — correct for real repos, but leaves noise directories unfiltered
// when cwd isn't a git repo at all. This is a small unconditional safety
// net on top of (not instead of) proper gitignore respect, not a return to
// the Node build's hardcoded-skip-list approach.
const ALWAYS_SKIP: [&str; 5] = [".git", "node_modules", "target", "dist", "build"];

/// A single matched line is capped at this many characters. Without it, one
/// match inside a minified bundle or a lockfile (which are a single enormous
/// line) produces a multi-megabyte result that then rides along in every
/// subsequent request of the agentic loop.
const MAX_MATCH_LINE_CHARS: usize = 300;

/// Ceiling on the searcher's own line buffer, so an enormous line is skipped
/// rather than fully materialised before we ever get to truncate it.
const SEARCHER_HEAP_LIMIT: usize = 1 << 20; // 1 MiB

fn truncate_match_line(line: &str) -> String {
    let trimmed = line.trim_end();
    if trimmed.chars().count() <= MAX_MATCH_LINE_CHARS {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(MAX_MATCH_LINE_CHARS).collect();
    format!("{head}… [line truncated]")
}

fn walker(cwd: &Path) -> ignore::Walk {
    WalkBuilder::new(cwd)
        .filter_entry(|entry| {
            !entry
                .file_name()
                .to_str()
                .map(|name| ALWAYS_SKIP.contains(&name))
                .unwrap_or(false)
        })
        // Deterministic order: the assembled prompt is a cache prefix, and a
        // filesystem-order walk silently reorders it between runs.
        .sort_by_file_name(|a, b| a.cmp(b))
        .build()
}

/// Full `path:line:content` matches — used by the `grep` tool, which needs
/// the line content itself.
pub fn search_text(pattern: &str, cwd: &Path, max_results: usize) -> Vec<String> {
    let matcher = match RegexMatcher::new(pattern) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };

    let mut results = Vec::new();
    // One searcher reused across every file, rather than a fresh line buffer
    // allocated per file walked (ripgrep's own usage pattern).
    let mut searcher = SearcherBuilder::new().heap_limit(Some(SEARCHER_HEAP_LIMIT)).build();

    for entry in walker(cwd) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let rel_path = path.strip_prefix(cwd).unwrap_or(path).to_string_lossy().into_owned();

        let search_result = searcher.search_path(
            &matcher,
            path,
            UTF8(|line_num, line| {
                results.push(format!("{rel_path}:{line_num}:{}", truncate_match_line(line)));
                Ok(results.len() < max_results)
            }),
        );
        let _ = search_result; // binary/unreadable/over-limit files yield no matches, not an error

        if results.len() >= max_results {
            break;
        }
    }

    results
}

/// Paths of files containing at least one match, capped on **files** rather
/// than on matching lines, and stopping at the first match within each file.
///
/// `search_text` caps on total matching lines, so ten matches inside one file
/// exhaust the budget and the caller learns about exactly one file. Context
/// discovery only ever wanted the path set, so it pays for neither the extra
/// scanning nor the line formatting it immediately threw away.
pub fn search_files(pattern: &str, cwd: &Path, max_files: usize) -> Vec<String> {
    let matcher = match RegexMatcher::new(pattern) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };

    let mut files = Vec::new();
    let mut searcher = SearcherBuilder::new().heap_limit(Some(SEARCHER_HEAP_LIMIT)).build();

    for entry in walker(cwd) {
        if files.len() >= max_files {
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let rel_path = path.strip_prefix(cwd).unwrap_or(path).to_string_lossy().into_owned();

        let mut matched = false;
        let _ = searcher.search_path(
            &matcher,
            path,
            UTF8(|_, _| {
                matched = true;
                Ok(false) // first match is enough — stop reading this file
            }),
        );
        if matched {
            files.push(rel_path);
        }
    }

    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // ignore::WalkBuilder correctly does NOT hardcode a skip-list (unlike the
    // Node build's fallback naive walker) — it only skips what an actual
    // .gitignore says. That in turn only activates inside a real git repo
    // by default (`require_git: true`, confirmed in ignore-0.4.33's
    // src/dir.rs) — which is exactly right for our real use case (cwd in
    // worker-tier usage is always a real repo), so the fixture is a real
    // git repo, not just a directory with a .gitignore file sitting in it.
    fn fixture() -> tempfile_dir::TempDir {
        let dir = tempfile_dir::TempDir::new();
        std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(dir.path())
            .output()
            .unwrap();
        fs::write(dir.path().join(".gitignore"), "node_modules/\n").unwrap();
        fs::write(dir.path().join("a.js"), "const needle = 1;\nconst other = 2;\n").unwrap();
        fs::create_dir(dir.path().join("node_modules")).unwrap();
        fs::write(dir.path().join("node_modules").join("b.js"), "const needle = 999;\n").unwrap();
        dir
    }

    #[test]
    fn finds_a_matching_line_with_file_line_content_format() {
        let dir = fixture();
        let results = search_text("needle", dir.path(), 50);
        assert_eq!(results.len(), 1);
        assert!(results[0].starts_with("a.js:1:"));
    }

    #[test]
    fn skips_gitignore_style_ignored_directories_like_node_modules_when_a_gitignore_exists() {
        let dir = fixture();
        let results = search_text("needle", dir.path(), 50);
        assert!(!results.iter().any(|r| r.contains("node_modules")));
    }

    #[test]
    fn always_skip_list_applies_even_outside_a_git_repo() {
        // The gap this test guards: outside a git repo, .gitignore never
        // applies (require_git: true), so without ALWAYS_SKIP, build/dep
        // directories would get scanned — confirmed live against this very
        // project's own target/ directory during development.
        let dir = tempfile_dir::TempDir::new();
        fs::write(dir.path().join("a.js"), "const needle = 1;\n").unwrap();
        fs::create_dir(dir.path().join("target")).unwrap();
        fs::write(dir.path().join("target").join("b.js"), "const needle = 999;\n").unwrap();
        let results = search_text("needle", dir.path(), 50);
        assert_eq!(results.len(), 1);
        assert!(!results.iter().any(|r| r.contains("target")));
    }

    #[test]
    fn does_not_assume_arbitrary_directories_are_excluded_without_a_gitignore() {
        // Only the small ALWAYS_SKIP list and real .gitignore rules exclude
        // anything — an arbitrary directory name outside both is walked
        // normally, unlike the Node build's broader hardcoded skip-list.
        let dir = tempfile_dir::TempDir::new();
        fs::write(dir.path().join("a.js"), "const needle = 1;\n").unwrap();
        fs::create_dir(dir.path().join("vendor")).unwrap();
        fs::write(dir.path().join("vendor").join("b.js"), "const needle = 999;\n").unwrap();
        let results = search_text("needle", dir.path(), 50);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn returns_no_results_not_an_error_for_a_pattern_that_does_not_match() {
        let dir = fixture();
        let results = search_text("nonexistent_pattern_xyz", dir.path(), 50);
        assert!(results.is_empty());
    }

    #[test]
    fn search_files_caps_on_files_not_on_matching_lines() {
        // The bug this guards: search_text caps on total matching *lines*, so
        // a file with many matches exhausts the budget and the caller only
        // learns about one file. search_files must return both files here.
        let dir = tempfile_dir::TempDir::new();
        fs::write(dir.path().join("many.js"), "needle\n".repeat(50)).unwrap();
        fs::write(dir.path().join("one.js"), "needle\n").unwrap();

        let lines = search_text("needle", dir.path(), 10);
        assert_eq!(lines.len(), 10, "search_text caps on lines");
        assert!(
            lines.iter().all(|l| l.starts_with("many.js:")),
            "one file's matches consume the whole line budget: {lines:?}"
        );

        let mut files = search_files("needle", dir.path(), 10);
        files.sort();
        assert_eq!(files, vec!["many.js".to_string(), "one.js".to_string()]);
    }

    #[test]
    fn search_files_respects_its_file_cap_and_finds_nothing_for_no_match() {
        let dir = tempfile_dir::TempDir::new();
        for i in 0..5 {
            fs::write(dir.path().join(format!("f{i}.js")), "needle\n").unwrap();
        }
        assert_eq!(search_files("needle", dir.path(), 3).len(), 3);
        assert!(search_files("nonexistent_pattern_xyz", dir.path(), 10).is_empty());
    }

    #[test]
    fn a_single_enormous_line_cannot_produce_an_enormous_match() {
        // A minified bundle is one huge line; without a cap a single match
        // becomes a multi-megabyte tool result that is then re-sent on every
        // subsequent turn of the agentic loop.
        let dir = tempfile_dir::TempDir::new();
        let huge = format!("needle{}", "x".repeat(100_000));
        fs::write(dir.path().join("bundle.min.js"), huge).unwrap();

        let results = search_text("needle", dir.path(), 50);
        assert_eq!(results.len(), 1);
        assert!(
            results[0].chars().count() < MAX_MATCH_LINE_CHARS + 100,
            "match line was not truncated: {} chars",
            results[0].chars().count()
        );
        assert!(results[0].contains("[line truncated]"));
    }

    #[test]
    fn walk_order_is_deterministic() {
        // The assembled prompt is a cache prefix; a filesystem-order walk
        // silently reorders it between runs and costs every cache hit.
        let dir = tempfile_dir::TempDir::new();
        for name in ["c.js", "a.js", "b.js"] {
            fs::write(dir.path().join(name), "needle\n").unwrap();
        }
        let first = search_files("needle", dir.path(), 10);
        let second = search_files("needle", dir.path(), 10);
        assert_eq!(first, second);
        assert_eq!(first, vec!["a.js".to_string(), "b.js".to_string(), "c.js".to_string()]);
    }

    #[test]
    fn respects_max_results() {
        let dir = tempfile_dir::TempDir::new();
        for i in 0..5 {
            fs::write(dir.path().join(format!("f{i}.txt")), "needle\n").unwrap();
        }
        let results = search_text("needle", dir.path(), 2);
        assert_eq!(results.len(), 2);
    }
}

// Minimal dependency-free temp-dir helper for this module's tests only
// (avoids adding the `tempfile` crate for four tests) — creates a unique
// dir under the OS temp dir and removes it on drop.
#[cfg(test)]
mod tempfile_dir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!("deepseek-axi-rust-ripgrep-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
