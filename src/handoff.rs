//! Agent-to-agent handoff schema — a faithful port of `handoff.js`. Own
//! contract, versioned from day one (§12/§17 of ENHANCEMENT_PLAN.md).
#![allow(dead_code)] // to_value()/render_summary(): part of the schema's public API surface, not every command uses every accessor

use serde::Serialize;
use serde_json::Value;

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Implemented,
    Verified,
    PartiallyVerified,
    Failed,
}

#[derive(Serialize, Clone, Default)]
pub struct FileChanged {
    pub path: String,
    pub insertions: u32,
    pub deletions: u32,
}

#[derive(Serialize, Clone, Default)]
pub struct TestSummary {
    pub run: u32,
    pub passed: u32,
    pub failed: u32,
    pub regressed: Vec<String>,
}

#[derive(Serialize, Clone, Default)]
pub struct Verification {
    #[serde(rename = "targetFixed")]
    pub target_fixed: bool,
    #[serde(rename = "noRegressions")]
    pub no_regressions: bool,
    #[serde(rename = "regressionCheckSkipped")]
    pub regression_check_skipped: bool,
}

#[derive(Serialize, Clone, Default)]
pub struct Cost {
    pub iterations: u32,
    pub model: Option<String>,
}

#[derive(Serialize, Clone, Default)]
pub struct UsageTotals {
    #[serde(rename = "promptTokens")]
    pub prompt_tokens: u64,
    #[serde(rename = "completionTokens")]
    pub completion_tokens: u64,
    #[serde(rename = "cacheHitTokens")]
    pub cache_hit_tokens: u64,
    #[serde(rename = "cacheMissTokens")]
    pub cache_miss_tokens: u64,
}

#[derive(Serialize, Clone)]
pub struct Handoff {
    pub status: Status,
    pub summary: String,
    #[serde(rename = "filesChanged")]
    pub files_changed: Vec<FileChanged>,
    pub tests: TestSummary,
    pub verification: Verification,
    pub risks: Vec<String>,
    #[serde(rename = "remainingWork")]
    pub remaining_work: Vec<String>,
    #[serde(rename = "nextActions")]
    pub next_actions: Vec<String>,
    pub cost: Cost,
    pub usage: UsageTotals,
    pub branch: Option<String>,
    #[serde(rename = "worktreePath")]
    pub worktree_path: Option<String>,
}

impl Handoff {
    pub fn builder(status: Status, summary: impl Into<String>) -> Self {
        Self {
            status,
            summary: summary.into(),
            files_changed: Vec::new(),
            tests: TestSummary::default(),
            verification: Verification::default(),
            risks: Vec::new(),
            remaining_work: Vec::new(),
            next_actions: Vec::new(),
            cost: Cost::default(),
            usage: UsageTotals::default(),
            branch: None,
            worktree_path: None,
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

pub fn render_json(handoff: &Handoff) -> String {
    serde_json::to_string_pretty(handoff).unwrap_or_default()
}

pub fn render_summary(handoff: &Handoff) -> String {
    handoff.summary.clone()
}

pub fn render_trimmed(handoff: &Handoff) -> Value {
    serde_json::json!({
        "status": handoff.status,
        "summary": handoff.summary,
        "filesChanged": handoff.files_changed,
        "verification": handoff.verification,
        "risks": handoff.risks,
        "remainingWork": handoff.remaining_work,
        "branch": handoff.branch,
        "cost": handoff.cost,
        "usage": handoff.usage,
        "help": handoff.next_actions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_fills_defaults_for_optional_fields() {
        let h = Handoff::builder(Status::Verified, "did the thing");
        assert_eq!(h.status, Status::Verified);
        assert!(h.files_changed.is_empty());
        assert!(h.risks.is_empty());
        assert_eq!(h.tests.run, 0);
    }

    #[test]
    fn render_json_round_trips_through_json_parse() {
        for status in [
            Status::Implemented,
            Status::Verified,
            Status::PartiallyVerified,
            Status::Failed,
        ] {
            let mut h = Handoff::builder(status, "x");
            h.branch = Some("deepseek-axi/abc123".into());
            h.files_changed.push(FileChanged {
                path: "a.js".into(),
                insertions: 1,
                deletions: 0,
            });
            let rendered = render_json(&h);
            let parsed: Value = serde_json::from_str(&rendered).expect("must be valid JSON");
            assert!(parsed.get("status").is_some());
            assert_eq!(
                parsed.get("branch").and_then(Value::as_str),
                Some("deepseek-axi/abc123")
            );
        }
    }

    #[test]
    fn render_summary_returns_just_the_prose_summary() {
        let h = Handoff::builder(Status::Verified, "the fix worked");
        assert_eq!(render_summary(&h), "the fix worked");
    }
}
