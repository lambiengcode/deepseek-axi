use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct TaskBucket {
    pub calls: u64,
    #[serde(rename = "hitTokens")]
    pub hit_tokens: u64,
    #[serde(rename = "missTokens")]
    pub miss_tokens: u64,
    #[serde(rename = "reasoningTokens")]
    pub reasoning_tokens: u64,
    #[serde(rename = "lastAt")]
    pub last_at: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Usage {
    pub calls: u64,
    #[serde(rename = "hitTokens")]
    pub hit_tokens: u64,
    #[serde(rename = "missTokens")]
    pub miss_tokens: u64,
    #[serde(rename = "reasoningTokens")]
    pub reasoning_tokens: u64,
    #[serde(rename = "lastModel")]
    pub last_model: Option<String>,
    #[serde(rename = "lastAt")]
    pub last_at: Option<String>,
    #[serde(rename = "byTask", default)]
    pub by_task: HashMap<String, TaskBucket>,
}

fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("DEEPSEEK_AXI_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("deepseek-axi")
}

fn usage_file() -> PathBuf {
    config_dir().join("usage.json")
}

pub fn read_usage() -> Usage {
    match fs::read_to_string(usage_file()) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Usage::default(),
    }
}

// ponytail: last-write-wins, no file lock — matches the Node build's own
// documented tradeoff; fine for a single-user interactive CLI.
pub fn record_usage(model: &str, usage_json: Option<&Value>, timestamp: String, task_id: Option<&str>) -> Usage {
    let mut current = read_usage();
    current.calls += 1;

    let hit = usage_json
        .and_then(|u| u.get("prompt_cache_hit_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let miss = usage_json
        .and_then(|u| u.get("prompt_cache_miss_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reasoning = usage_json
        .and_then(|u| u.get("completion_tokens_details"))
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    current.hit_tokens += hit;
    current.miss_tokens += miss;
    current.reasoning_tokens += reasoning;
    current.last_model = Some(model.to_string());
    current.last_at = Some(timestamp.clone());

    if let Some(tid) = task_id {
        let bucket = current.by_task.entry(tid.to_string()).or_default();
        bucket.calls += 1;
        bucket.hit_tokens += hit;
        bucket.miss_tokens += miss;
        bucket.reasoning_tokens += reasoning;
        bucket.last_at = Some(timestamp);
    }

    let dir = config_dir();
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(usage_file(), serde_json::to_string_pretty(&current).unwrap_or_default());
    current
}

pub fn hit_rate(hit: u64, miss: u64) -> String {
    let total = hit + miss;
    if total == 0 {
        "n/a".to_string()
    } else {
        format!("{:.1}%", (hit as f64 / total as f64) * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    // Tests mutate a process-global env var (DEEPSEEK_AXI_CONFIG_DIR), so
    // they must not run concurrently within this process — same hazard as
    // the Node build's module-caching pitfall, different mechanism.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn record_usage_accumulates_and_hit_rate_computes_correctly() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("deepseek-axi-rust-test-{}", std::process::id()));
        unsafe {
            std::env::set_var("DEEPSEEK_AXI_CONFIG_DIR", &dir);
        }

        record_usage(
            "deepseek-v4-flash",
            Some(&json!({"prompt_cache_hit_tokens": 80, "prompt_cache_miss_tokens": 20})),
            "t1".into(),
            None,
        );
        record_usage(
            "deepseek-v4-flash",
            Some(&json!({"prompt_cache_hit_tokens": 0, "prompt_cache_miss_tokens": 100})),
            "t2".into(),
            None,
        );

        let usage = read_usage();
        assert_eq!(usage.calls, 2);
        assert_eq!(usage.hit_tokens, 80);
        assert_eq!(usage.miss_tokens, 120);
        assert_eq!(hit_rate(usage.hit_tokens, usage.miss_tokens), "40.0%");

        unsafe {
            std::env::remove_var("DEEPSEEK_AXI_CONFIG_DIR");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_usage_segments_per_task() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("deepseek-axi-rust-test-task-{}", std::process::id()));
        unsafe {
            std::env::set_var("DEEPSEEK_AXI_CONFIG_DIR", &dir);
        }

        record_usage(
            "deepseek-v4-pro",
            Some(&json!({"prompt_cache_hit_tokens": 10, "prompt_cache_miss_tokens": 10})),
            "t1".into(),
            Some("task-a"),
        );
        record_usage(
            "deepseek-v4-flash",
            Some(&json!({"prompt_cache_hit_tokens": 1, "prompt_cache_miss_tokens": 1})),
            "t2".into(),
            None,
        );

        let usage = read_usage();
        assert_eq!(usage.calls, 2);
        assert_eq!(usage.by_task.len(), 1);
        assert_eq!(usage.by_task.get("task-a").unwrap().calls, 1);

        unsafe {
            std::env::remove_var("DEEPSEEK_AXI_CONFIG_DIR");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_usage_on_missing_file_returns_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("deepseek-axi-rust-test-missing-{}", std::process::id()));
        unsafe {
            std::env::set_var("DEEPSEEK_AXI_CONFIG_DIR", &dir);
        }
        let usage = read_usage();
        assert_eq!(usage.calls, 0);
        assert!(usage.by_task.is_empty());
        unsafe {
            std::env::remove_var("DEEPSEEK_AXI_CONFIG_DIR");
        }
    }
}
