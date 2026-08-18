//! Tool definitions the worker loop offers to the model so it can pull its
//! own context and apply edits mid-task instead of everything being
//! pre-computed/pre-applied by us. Mirrors `tools.js`, including its
//! path-containment fix (a model-issued `write_file`/`read_file` with a
//! `../../` path must not escape the worktree — the worktree is the safety
//! boundary).

use crate::error::CliError;
use crate::ripgrep::search_text;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn tool_defs() -> Value {
    json!([
        {"type": "function", "function": {"name": "read_file", "description": "Read a file at a given path, relative to the repo root. Returns 1-indexed numbered lines, so the numbers can be passed straight to fim_edit. Large files are paged: pass offset/limit to read a specific window.", "parameters": {"type": "object", "properties": {"path": {"type": "string", "description": "File path to read"}, "offset": {"type": "integer", "description": "1-indexed first line to return (default 1)"}, "limit": {"type": "integer", "description": "Maximum number of lines to return"}}, "required": ["path"]}}},
        {"type": "function", "function": {"name": "grep", "description": "Search for a regex pattern across files in the repo, returning matching lines. Optionally scope the search to a subdirectory.", "parameters": {"type": "object", "properties": {"pattern": {"type": "string", "description": "Regex pattern to search for"}, "path": {"type": "string", "description": "Directory to search under, relative to the repo root (default '.')"}}, "required": ["pattern"]}}},
        {"type": "function", "function": {"name": "list_files", "description": "List entries in a directory (non-recursive), relative to the repo root.", "parameters": {"type": "object", "properties": {"dir": {"type": "string", "description": "Directory to list (default '.')"}}, "required": []}}},
        {"type": "function", "function": {"name": "write_file", "description": "Write (create or overwrite) a file's full contents at a given path, relative to the repo root. Use for new files or changes spanning most of the file.", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "content"]}}},
        {"type": "function", "function": {"name": "fim_edit", "description": "Replace a contiguous line range in an existing file using DeepSeek's Fill-In-the-Middle completion, instead of rewriting the whole file. Prefer this over write_file for a small, localized fix (e.g. one function or a few lines) — it's cheaper and carries no risk of the model rewriting unrelated parts of the file, since everything outside start_line..end_line is passed through untouched.", "parameters": {"type": "object", "properties": {"path": {"type": "string", "description": "File to edit"}, "start_line": {"type": "integer", "description": "1-indexed first line of the range to replace"}, "end_line": {"type": "integer", "description": "1-indexed last line of the range to replace (inclusive)"}}, "required": ["path", "start_line", "end_line"]}}},
        {"type": "function", "function": {"name": "run_tests", "description": "Run the project's test command and report pass/fail plus output.", "parameters": {"type": "object", "properties": {}, "required": []}}},
    ])
}

/// Tools available in read-only (dry-run/plan-only) mode.
pub fn read_only_tool_defs() -> Value {
    let all = tool_defs();
    let filtered: Vec<Value> = all
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| {
            let name = t["function"]["name"].as_str().unwrap_or("");
            name != "write_file" && name != "run_tests" && name != "fim_edit"
        })
        .cloned()
        .collect();
    json!(filtered)
}

fn resolve_within_cwd(cwd: &Path, path: &str) -> Result<PathBuf, String> {
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    if !normalized.starts_with(cwd) {
        return Err(format!("Path escapes the working directory: {path}"));
    }
    Ok(normalized)
}

pub struct ToolContext<'a> {
    pub cwd: PathBuf,
    pub test_command: Option<String>,
    pub allow_write: bool,
    /// The model the loop is running with, so `fim_edit` bills at the tier
    /// the caller actually asked for. It previously hardcoded pro, silently
    /// defeating `run --model flash` on every edit.
    pub model: String,
    /// Deadline for a model-invoked `run_tests`. Without one, a hanging test
    /// command blocks the whole run: the loop's budget check only runs
    /// between model turns and cannot interrupt a blocking wait.
    pub test_timeout: Duration,
    /// Injected (not a direct `crate::api::completion` call) so tool
    /// execution stays unit-testable without hitting the network — same
    /// dependency-injection pattern as `LoopOptions.chat_completion_fn`.
    pub completion_fn: &'a dyn Fn(Value) -> Result<Value, CliError>,
}

/// Ceiling on any single tool result. Tool results are appended to the
/// conversation and re-sent on every subsequent turn, so one unbounded result
/// is paid for repeatedly, not once.
const MAX_TOOL_RESULT_CHARS: usize = 24_000;
/// Default page size for `read_file` when the model doesn't ask for one.
const READ_FILE_DEFAULT_LINES: usize = 2_000;

/// Renders file content as 1-indexed numbered lines.
///
/// `fim_edit` requires exact line numbers and `grep` already returns them, so
/// handing the model an unnumbered blob forced it to count newlines by hand —
/// and a miscount silently splices the wrong region.
fn number_lines(content: &str, first_line: usize) -> String {
    content
        .lines()
        .enumerate()
        .map(|(i, line)| format!("{}\t{}", first_line + i, line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Splits `content` at line boundaries into (prefix, suffix) around the
/// inclusive 1-indexed `[start_line, end_line]` range, as byte-offset
/// substrings of the original — preserves exact formatting (trailing
/// newlines, CRLF, etc.) rather than reconstructing via line-join, which is
/// lossy at file boundaries.
fn split_for_fim(content: &str, start_line: u64, end_line: u64) -> Result<(&str, &str), String> {
    if start_line == 0 || end_line < start_line {
        return Err("start_line must be >= 1 and end_line >= start_line".to_string());
    }
    let mut line_starts = vec![0usize];
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let start_idx = (start_line - 1) as usize;
    if start_idx >= line_starts.len() {
        return Err(format!("start_line {start_line} is past the end of the file"));
    }
    let end_idx = (end_line - 1) as usize;
    let prefix = &content[..line_starts[start_idx]];
    let suffix_start = line_starts.get(end_idx + 1).copied().unwrap_or(content.len());
    let suffix = &content[suffix_start..];
    Ok((prefix, suffix))
}

/// Executes one tool call. Never panics — malformed arguments, an unknown
/// tool name, or a path-containment violation all become an `{error}`
/// result the model can react to, not a crash.
pub fn execute_tool_call(name: &str, args: &Value, ctx: &ToolContext) -> Value {
    if !ctx.allow_write && (name == "write_file" || name == "run_tests" || name == "fim_edit") {
        return json!({"error": format!("Unknown tool: {name}")});
    }

    match name {
        "read_file" => {
            let Some(path) = args.get("path").and_then(Value::as_str) else {
                return json!({"error": "missing path"});
            };
            let content = match resolve_within_cwd(&ctx.cwd, path)
                .and_then(|p| std::fs::read_to_string(&p).map_err(|e| e.to_string()))
            {
                Ok(c) => c,
                Err(e) => return json!({"error": e}),
            };

            let total_lines = content.lines().count();
            let offset = args
                .get("offset")
                .and_then(Value::as_u64)
                .map(|o| (o.max(1) - 1) as usize)
                .unwrap_or(0);
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map(|l| l as usize)
                .unwrap_or(READ_FILE_DEFAULT_LINES);

            if offset >= total_lines && total_lines > 0 {
                return json!({"error": format!("offset {} is past the end of the file ({total_lines} lines)", offset + 1)});
            }

            let window: String = content.lines().skip(offset).take(limit).collect::<Vec<_>>().join("\n");
            let returned = window.lines().count();
            let numbered = number_lines(&window, offset + 1);
            let truncated_by_chars = numbered.chars().count() > MAX_TOOL_RESULT_CHARS;
            let body = if truncated_by_chars {
                crate::files::head_tail(&numbered, MAX_TOOL_RESULT_CHARS / 2, MAX_TOOL_RESULT_CHARS / 2)
            } else {
                numbered
            };

            let complete = offset == 0 && returned == total_lines && !truncated_by_chars;
            if complete {
                json!({"content": body, "totalLines": total_lines})
            } else {
                json!({
                    "content": body,
                    "totalLines": total_lines,
                    "truncated": true,
                    "hint": format!("showing lines {}-{} of {total_lines}; re-read with offset/limit for more", offset + 1, offset + returned),
                })
            }
        }
        "grep" => {
            let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
                return json!({"error": "missing pattern"});
            };
            let scope = args.get("path").and_then(Value::as_str).unwrap_or(".");
            let root = match resolve_within_cwd(&ctx.cwd, scope) {
                Ok(p) => p,
                Err(e) => return json!({"error": e}),
            };
            const GREP_MAX: usize = 50;
            let matches = search_text(pattern, &root, GREP_MAX);
            // Tell the model when it only saw part of the picture, so it
            // narrows the search instead of assuming the list is exhaustive.
            if matches.len() >= GREP_MAX {
                json!({"matches": matches, "truncated": true, "hint": format!("stopped at {GREP_MAX} matches; narrow the pattern or pass path to scope the search")})
            } else {
                json!({"matches": matches})
            }
        }
        "list_files" => {
            let dir = args.get("dir").and_then(Value::as_str).unwrap_or(".");
            match resolve_within_cwd(&ctx.cwd, dir) {
                Ok(p) => match std::fs::read_dir(&p) {
                    Ok(entries) => {
                        let names: Vec<String> = entries
                            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                            .collect();
                        json!({"files": names})
                    }
                    Err(e) => json!({"error": e.to_string()}),
                },
                Err(e) => json!({"error": e}),
            }
        }
        "write_file" => {
            let (Some(path), Some(content)) = (
                args.get("path").and_then(Value::as_str),
                args.get("content").and_then(Value::as_str),
            ) else {
                return json!({"error": "missing path or content"});
            };
            match resolve_within_cwd(&ctx.cwd, path) {
                Ok(p) => {
                    if let Some(parent) = p.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    match std::fs::write(&p, content) {
                        Ok(_) => json!({"written": true, "path": path}),
                        Err(e) => json!({"error": e.to_string()}),
                    }
                }
                Err(e) => json!({"error": e}),
            }
        }
        "fim_edit" => {
            let (Some(path), Some(start_line), Some(end_line)) = (
                args.get("path").and_then(Value::as_str),
                args.get("start_line").and_then(Value::as_u64),
                args.get("end_line").and_then(Value::as_u64),
            ) else {
                return json!({"error": "missing path, start_line, or end_line"});
            };

            let resolved = match resolve_within_cwd(&ctx.cwd, path) {
                Ok(p) => p,
                Err(e) => return json!({"error": e}),
            };
            let content = match std::fs::read_to_string(&resolved) {
                Ok(c) => c,
                Err(e) => return json!({"error": e.to_string()}),
            };
            let (prefix, suffix) = match split_for_fim(&content, start_line, end_line) {
                Ok(v) => v,
                Err(e) => return json!({"error": e}),
            };

            let payload = json!({"model": ctx.model, "prompt": prefix, "suffix": suffix, "max_tokens": 512});
            let response = match (ctx.completion_fn)(payload) {
                Ok(r) => r,
                Err(e) => return json!({"error": format!("FIM completion failed: {}", e.message)}),
            };
            let filled = response
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("");

            // FIM models fill up to wherever the given suffix naturally
            // continues — since `suffix` starts exactly at the next line's
            // first byte (no leading newline), a model that stops right at
            // the line boundary can omit the newline entirely, splicing two
            // lines together (confirmed live: "}" + "module.exports..." ->
            // "}module.exports..." with no separator). Guard the boundary
            // explicitly rather than trust the model's completion to supply it.
            let mut new_content = format!("{prefix}{filled}");
            if !suffix.is_empty() && !new_content.ends_with('\n') && !suffix.starts_with('\n') {
                new_content.push('\n');
            }
            new_content.push_str(suffix);
            if let Err(e) = std::fs::write(&resolved, &new_content) {
                return json!({"error": e.to_string()});
            }

            // Echo the spliced region back with line numbers. The write is
            // unconditional, so without this an off-by-N range silently
            // rewrites the wrong lines and the model's only way to notice is
            // to re-read the entire file.
            let filled_lines = filled.lines().count().max(1);
            let context_start = start_line.saturating_sub(3).max(1) as usize;
            let context_end = (start_line as usize - 1) + filled_lines + 3;
            let window: String = new_content
                .lines()
                .skip(context_start - 1)
                .take(context_end - context_start + 1)
                .collect::<Vec<_>>()
                .join("\n");
            json!({
                "written": true,
                "path": path,
                "linesReplaced": format!("{start_line}-{end_line}"),
                "result": number_lines(&window, context_start),
                "hint": "verify the spliced region above matches your intent before moving on",
            })
        }
        "run_tests" => {
            let command = ctx
                .test_command
                .clone()
                .or_else(|| crate::worker::verify::detect_test_command(&ctx.cwd));
            let Some(command) = command else {
                return json!({
                    "passed": false,
                    "output": "No test command detected. Re-run deepseek-axi with --test-cmd \"<command>\".",
                });
            };
            let result = crate::worker::verify::run_tests(&ctx.cwd, Some(&command), ctx.test_timeout);
            json!({
                "passed": result.passed,
                "timedOut": result.timed_out,
                // Keep the tail: the assertion and stack trace live at the
                // end, and this result rides along in every later turn.
                "output": crate::files::head_tail(&result.output, 2_000, MAX_TOOL_RESULT_CHARS - 2_000),
            })
        }
        other => json!({"error": format!("Unknown tool: {other}")}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn noop_completion_fn(_: Value) -> Result<Value, CliError> {
        Ok(json!({"choices": [{"text": ""}]}))
    }

    fn ctx(cwd: PathBuf) -> ToolContext<'static> {
        ToolContext {
            cwd,
            test_command: Some("true".into()),
            allow_write: true,
            model: crate::models::MODEL_PRO.to_string(),
            test_timeout: std::time::Duration::from_secs(30),
            completion_fn: &noop_completion_fn,
        }
    }

    #[test]
    fn tool_defs_declares_all_six_tools() {
        let defs = tool_defs();
        let names: Vec<&str> = defs
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["read_file", "grep", "list_files", "write_file", "fim_edit", "run_tests"]
        );
    }

    #[test]
    fn read_only_tool_defs_excludes_all_mutating_tools() {
        let defs = read_only_tool_defs();
        let names: Vec<&str> = defs
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(!names.contains(&"write_file"));
        assert!(!names.contains(&"run_tests"));
        assert!(!names.contains(&"fim_edit"));
    }

    #[test]
    fn every_mutating_tool_offered_when_allow_write_is_also_rejected_when_it_is_false() {
        // Guards the exact bug found while wiring fim_edit: read_only_tool_defs()
        // (what's *offered*) and execute_tool_call's allow_write gate (what's
        // *usable*) must agree, or dry-run mode offers a tool it then rejects.
        let offered_when_writable: Vec<String> = tool_defs()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect();
        let offered_read_only: Vec<String> = read_only_tool_defs()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect();
        let mutating: Vec<&String> = offered_when_writable
            .iter()
            .filter(|n| !offered_read_only.contains(n))
            .collect();

        let dir = std::env::temp_dir().join(format!("deepseek-axi-rust-tools-parity-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let read_only_ctx = ToolContext {
            cwd: dir.clone(),
            test_command: None,
            allow_write: false,
            model: crate::models::MODEL_PRO.to_string(),
            test_timeout: std::time::Duration::from_secs(30),
            completion_fn: &noop_completion_fn,
        };
        for name in mutating {
            let result = execute_tool_call(name, &json!({}), &read_only_ctx);
            assert!(
                result["error"].as_str().unwrap().contains("Unknown tool"),
                "{name} should be rejected when allow_write is false"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_file_returns_numbered_lines_so_fim_edit_ranges_are_not_guesswork() {
        let dir = std::env::temp_dir().join(format!("deepseek-axi-rust-tools-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let result = execute_tool_call("read_file", &json!({"path": "a.txt"}), &ctx(dir.clone()));
        assert_eq!(result["content"], "1\talpha\n2\tbeta\n3\tgamma");
        assert_eq!(result["totalLines"], 3);
        // Whole file returned, so no truncation marker to confuse the model.
        assert!(result.get("truncated").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_file_pages_with_offset_and_limit_and_says_so() {
        let dir = std::env::temp_dir().join(format!("deepseek-axi-rust-tools-page-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let body: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        fs::write(dir.join("a.txt"), body).unwrap();

        let result = execute_tool_call(
            "read_file",
            &json!({"path": "a.txt", "offset": 4, "limit": 2}),
            &ctx(dir.clone()),
        );
        // Numbering stays absolute, so the model can hand these straight to fim_edit.
        assert_eq!(result["content"], "4\tline4\n5\tline5");
        assert_eq!(result["totalLines"], 10);
        assert_eq!(result["truncated"], true);
        assert!(result["hint"].as_str().unwrap().contains("lines 4-5 of 10"));

        let past_end = execute_tool_call("read_file", &json!({"path": "a.txt", "offset": 999}), &ctx(dir.clone()));
        assert!(past_end["error"].as_str().unwrap().contains("past the end"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn grep_flags_truncation_so_the_model_narrows_instead_of_assuming() {
        let dir = std::env::temp_dir().join(format!("deepseek-axi-rust-tools-grep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("many.txt"), "needle\n".repeat(80)).unwrap();

        let result = execute_tool_call("grep", &json!({"pattern": "needle"}), &ctx(dir.clone()));
        assert_eq!(result["matches"].as_array().unwrap().len(), 50);
        assert_eq!(result["truncated"], true);

        // A scoped search must stay inside the worktree.
        let escaped = execute_tool_call(
            "grep",
            &json!({"pattern": "needle", "path": "../.."}),
            &ctx(dir.clone()),
        );
        assert!(escaped["error"].as_str().unwrap().contains("escapes"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_file_creates_nested_file_inside_cwd() {
        let dir = std::env::temp_dir().join(format!("deepseek-axi-rust-tools-write-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let result = execute_tool_call(
            "write_file",
            &json!({"path": "nested/out.txt", "content": "written"}),
            &ctx(dir.clone()),
        );
        assert_eq!(result["written"], true);
        assert_eq!(
            fs::read_to_string(dir.join("nested").join("out.txt")).unwrap(),
            "written"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_and_write_reject_paths_that_escape_cwd() {
        let dir = std::env::temp_dir().join(format!("deepseek-axi-rust-tools-escape-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let r1 = execute_tool_call("read_file", &json!({"path": "../../etc/passwd"}), &ctx(dir.clone()));
        assert!(r1["error"].as_str().unwrap().contains("escapes"));
        let r2 = execute_tool_call(
            "write_file",
            &json!({"path": "../../tmp/evil.txt", "content": "x"}),
            &ctx(dir.clone()),
        );
        assert!(r2["error"].as_str().unwrap().contains("escapes"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_file_and_run_tests_return_unknown_tool_when_allow_write_is_false() {
        let dir = std::env::temp_dir().join(format!("deepseek-axi-rust-tools-noWrite-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let read_only_ctx = ToolContext {
            cwd: dir.clone(),
            test_command: None,
            allow_write: false,
            model: crate::models::MODEL_PRO.to_string(),
            test_timeout: std::time::Duration::from_secs(30),
            completion_fn: &noop_completion_fn,
        };
        let r1 = execute_tool_call("write_file", &json!({"path": "x.txt", "content": "x"}), &read_only_ctx);
        assert!(r1["error"].as_str().unwrap().contains("Unknown tool"));
        let r2 = execute_tool_call("run_tests", &json!({}), &read_only_ctx);
        assert!(r2["error"].as_str().unwrap().contains("Unknown tool"));
        let r3 = execute_tool_call(
            "fim_edit",
            &json!({"path": "x.txt", "start_line": 1, "end_line": 1}),
            &read_only_ctx,
        );
        assert!(r3["error"].as_str().unwrap().contains("Unknown tool"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_tool_name_returns_error_object() {
        let dir = std::env::temp_dir();
        let result = execute_tool_call("delete_everything", &json!({}), &ctx(dir));
        assert!(result["error"].as_str().unwrap().contains("Unknown tool"));
    }

    #[test]
    fn split_for_fim_preserves_exact_surrounding_bytes() {
        let content = "line1\nline2\nline3\nline4\n";
        let (prefix, suffix) = split_for_fim(content, 2, 3).unwrap();
        assert_eq!(prefix, "line1\n");
        assert_eq!(suffix, "line4\n");
    }

    #[test]
    fn split_for_fim_handles_the_first_line_and_last_line() {
        let content = "a\nb\nc\n";
        let (prefix, suffix) = split_for_fim(content, 1, 3).unwrap();
        assert_eq!(prefix, "");
        assert_eq!(suffix, "");
    }

    #[test]
    fn split_for_fim_rejects_invalid_ranges() {
        assert!(split_for_fim("a\nb\n", 0, 1).is_err());
        assert!(split_for_fim("a\nb\n", 2, 1).is_err());
        assert!(split_for_fim("a\nb\n", 99, 100).is_err());
    }

    #[test]
    fn fim_edit_replaces_only_the_requested_line_range() {
        let dir = std::env::temp_dir().join(format!("deepseek-axi-rust-tools-fim-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("math.js"),
            "function add(a, b) {\n  return a + b + 1; // bug\n}\n",
        )
        .unwrap();

        fn fixed_completion_fn(payload: Value) -> Result<Value, CliError> {
            assert_eq!(payload["model"], "deepseek-v4-pro");
            assert!(
                payload["prompt"]
                    .as_str()
                    .unwrap()
                    .starts_with("function add(a, b) {\n")
            );
            assert!(payload["suffix"].as_str().unwrap().starts_with("}\n"));
            Ok(json!({"choices": [{"text": "  return a + b;"}]}))
        }
        let ctx = ToolContext {
            cwd: dir.clone(),
            test_command: None,
            allow_write: true,
            model: crate::models::MODEL_PRO.to_string(),
            test_timeout: std::time::Duration::from_secs(30),
            completion_fn: &fixed_completion_fn,
        };

        let result = execute_tool_call(
            "fim_edit",
            &json!({"path": "math.js", "start_line": 2, "end_line": 2}),
            &ctx,
        );
        assert_eq!(result["written"], true);
        let new_content = fs::read_to_string(dir.join("math.js")).unwrap();
        // Note the inserted "\n" before "}": the stub's filled text has no
        // trailing newline and the suffix ("}\n...") has no leading one —
        // the boundary-normalization fix (below) supplies the separator
        // that a real FIM completion isn't guaranteed to include itself.
        assert_eq!(new_content, "function add(a, b) {\n  return a + b;\n}\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fim_edit_inserts_a_newline_at_the_fill_suffix_boundary_when_neither_side_has_one() {
        // Regression test for a real bug found live: DeepSeek's FIM
        // completion filled right up to the suffix boundary with no
        // trailing newline, and the suffix had no leading newline either,
        // splicing two lines together ("}" + "module.exports..." ->
        // "}module.exports..." with zero separator) on an actual live run.
        let dir = std::env::temp_dir().join(format!("deepseek-axi-rust-tools-fim-boundary-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("math.js"),
            "function multiply(a, b) {\n  return a * b + 1; // bug\n}\nmodule.exports = { multiply };\n",
        )
        .unwrap();

        fn no_trailing_newline_completion_fn(_: Value) -> Result<Value, CliError> {
            // Mirrors the real observed completion: fills through the
            // closing brace with no trailing "\n".
            Ok(json!({"choices": [{"text": "  return a * b;\n}"}]}))
        }
        let ctx = ToolContext {
            cwd: dir.clone(),
            test_command: None,
            allow_write: true,
            model: crate::models::MODEL_PRO.to_string(),
            test_timeout: std::time::Duration::from_secs(30),
            completion_fn: &no_trailing_newline_completion_fn,
        };

        // Replacing lines 2-3 ("return a * b + 1;" and "}") — suffix starts
        // directly at "module.exports...", no leading newline.
        let result = execute_tool_call(
            "fim_edit",
            &json!({"path": "math.js", "start_line": 2, "end_line": 3}),
            &ctx,
        );
        assert_eq!(result["written"], true);
        let new_content = fs::read_to_string(dir.join("math.js")).unwrap();
        assert_eq!(
            new_content,
            "function multiply(a, b) {\n  return a * b;\n}\nmodule.exports = { multiply };\n"
        );
        assert!(
            !new_content.contains("}module.exports"),
            "must not splice lines together with no separator"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fim_edit_surfaces_completion_errors_without_writing() {
        let dir = std::env::temp_dir().join(format!("deepseek-axi-rust-tools-fim-err-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.js"), "x\ny\nz\n").unwrap();

        fn failing_completion_fn(_: Value) -> Result<Value, CliError> {
            Err(CliError::new("network down", "NETWORK_ERROR", vec![]))
        }
        let ctx = ToolContext {
            cwd: dir.clone(),
            test_command: None,
            allow_write: true,
            model: crate::models::MODEL_PRO.to_string(),
            test_timeout: std::time::Duration::from_secs(30),
            completion_fn: &failing_completion_fn,
        };

        let result = execute_tool_call(
            "fim_edit",
            &json!({"path": "a.js", "start_line": 2, "end_line": 2}),
            &ctx,
        );
        assert!(result["error"].as_str().unwrap().contains("network down"));
        assert_eq!(fs::read_to_string(dir.join("a.js")).unwrap(), "x\ny\nz\n"); // untouched
        let _ = fs::remove_dir_all(&dir);
    }
}
