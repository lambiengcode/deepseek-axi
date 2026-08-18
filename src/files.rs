use crate::error::CliError;
use std::fs;

/// Keeps both ends of a long text, eliding the middle.
///
/// The failure a model needs is at the *end* of a test run — the assertion
/// diff and the stack trace — while the beginning is the framework banner and
/// the tests that passed. Plain head truncation therefore spent the entire
/// budget showing the model everything except the thing it was asked to
/// diagnose.
pub fn head_tail(text: &str, head: usize, tail: usize) -> String {
    let total = text.chars().count();
    if total <= head + tail {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let elided = total - head - tail;
    let start: String = chars[..head].iter().collect();
    let end: String = chars[total - tail..].iter().collect();
    format!("{start}\n… [{elided} characters elided] …\n{end}")
}

/// Inlines file contents into a prompt block instead of making the calling
/// agent read the file into its own context just to paste it back in.
pub fn read_files_for_prompt(paths: &[String]) -> Result<String, CliError> {
    let mut blocks = Vec::with_capacity(paths.len());
    for path in paths {
        let content = fs::read_to_string(path)
            .map_err(|e| CliError::validation(format!("Could not read file: {path}"), vec![e.to_string()]))?;
        blocks.push(format!("--- {path} ---\n{content}"));
    }
    Ok(blocks.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_tail_keeps_the_failure_at_the_end() {
        let output = format!("{}ASSERTION FAILED", "pass\n".repeat(200));
        let kept = head_tail(&output, 20, 40);
        assert!(kept.contains("ASSERTION FAILED"), "the tail is the whole point");
        assert!(kept.starts_with("pass"));
        assert!(kept.contains("characters elided"));
        assert!(kept.chars().count() < output.chars().count());
    }

    #[test]
    fn head_tail_returns_short_text_unchanged() {
        assert_eq!(head_tail("short", 100, 100), "short");
        // Exactly at the boundary is still unchanged, not elided.
        assert_eq!(head_tail("abcd", 2, 2), "abcd");
    }

    #[test]
    fn head_tail_is_char_safe_on_multibyte_input() {
        let text = "→".repeat(100);
        let kept = head_tail(&text, 5, 5);
        assert!(kept.starts_with("→→→→→"));
        assert!(kept.ends_with("→→→→→"));
    }
}
