use std::fmt;

/// Structured error type, matching the JS build's `AxiError`: a message, a
/// machine-readable code, and human-readable suggestions. `VALIDATION_ERROR`
/// exits 2; everything else exits 1 — the same convention as the Node build
/// and as `axi-sdk-js`'s own `exitCodeForError`.
#[derive(Debug, Clone)]
pub struct CliError {
    pub message: String,
    pub code: &'static str,
    pub suggestions: Vec<String>,
}

impl CliError {
    pub fn new(message: impl Into<String>, code: &'static str, suggestions: Vec<String>) -> Self {
        Self {
            message: message.into(),
            code,
            suggestions,
        }
    }

    pub fn validation(message: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::new(message, "VALIDATION_ERROR", suggestions)
    }

    pub fn exit_code(&self) -> i32 {
        if self.code == "VALIDATION_ERROR" { 2 } else { 1 }
    }

    pub fn to_toon_value(&self) -> serde_json::Value {
        let mut obj = serde_json::json!({
            "error": self.message,
            "code": self.code,
        });
        if !self.suggestions.is_empty() {
            obj["help"] = serde_json::json!(self.suggestions);
        }
        obj
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CliError {}
