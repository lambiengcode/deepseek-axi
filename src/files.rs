use crate::error::CliError;
use std::fs;

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
