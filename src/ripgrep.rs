//! File-content search backed directly by ripgrep's own library crates
//! (`grep` + `ignore`) instead of shelling out to a separately-installed
//! `rg` binary or a hand-rolled walker — the concrete Rust-specific win
//! from RUST_MIGRATION_PLAN.md §1 (SIMD-accelerated transitively via
//! `memchr`/`regex` inside these crates, and `ignore` respects .gitignore
//! correctly instead of a hardcoded skip-list).

use grep::regex::RegexMatcher;
use grep::searcher::Searcher;
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

pub fn search_text(pattern: &str, cwd: &Path, max_results: usize) -> Vec<String> {
    let matcher = match RegexMatcher::new(pattern) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };

    let mut results = Vec::new();
    let walker = WalkBuilder::new(cwd)
        .filter_entry(|entry| {
            !entry
                .file_name()
                .to_str()
                .map(|name| ALWAYS_SKIP.contains(&name))
                .unwrap_or(false)
        })
        .build();

    'files: for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let rel_path = path.strip_prefix(cwd).unwrap_or(path).to_string_lossy().into_owned();

        let mut searcher = Searcher::new();
        let search_result = searcher.search_path(
            &matcher,
            path,
            UTF8(|line_num, line| {
                results.push(format!("{rel_path}:{line_num}:{}", line.trim_end()));
                Ok(results.len() < max_results)
            }),
        );
        let _ = search_result; // binary/unreadable files just yield no matches, not an error

        if results.len() >= max_results {
            break 'files;
        }
    }

    results
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
