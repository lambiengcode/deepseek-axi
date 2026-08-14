//! TOON output rendering.
//!
//! Phase 0 decision (RUST_MIGRATION_PLAN.md §8, spike run 2026-08-14): the
//! `toon-format` crate (v0.5.0, officially orged under `toon-format`, targets
//! TOON spec v3.0) produces output that is semantically identical to but not
//! byte-identical with what axi-sdk-js's JS encoder (`@toon-format/toon@2.3.1`)
//! produces for the same data — the crate quotes strings containing
//! colons/commas (e.g. `"default: chat, streaming"`) where the JS encoder
//! leaves them bare. This is a formatting/escaping variance, not a data-loss
//! bug: the crate's behavior is the more conservative/unambiguous one for a
//! CSV-like tabular format. Decision: **adopt the crate as-is** — this tool's
//! output only needs to be internally consistent, not byte-identical to its
//! JS-based sibling tools. See `flat_object_matches_known_node_shape` et al.
//! below for the comparison this decision was based on.

use serde::Serialize;
use toon_format::EncodeOptions;

pub fn render<T: Serialize>(value: &T) -> String {
    toon_format::encode(value, &EncodeOptions::default()).unwrap_or_else(|e| format!("error: TOON encode failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flat_object_matches_known_node_shape_semantically() {
        let value = json!({"apiKey": "missing", "defaultModel": "deepseek-v4-flash", "calls": 0});
        let encoded = render(&value);
        assert!(encoded.contains("apiKey: missing"));
        assert!(encoded.contains("deepseek-v4-flash"));
        assert!(encoded.contains("calls: 0"));
    }

    #[test]
    fn array_of_objects_renders_as_tabular_toon() {
        let value = json!({
            "count": 2,
            "models": [
                {"model": "deepseek-v4-flash", "context": "1M"},
                {"model": "deepseek-v4-pro", "context": "1M"}
            ]
        });
        let encoded = render(&value);
        assert!(encoded.contains("models[2]{model,context}:"));
    }

    #[test]
    fn string_array_renders_with_count_prefix() {
        let value = json!({"help": ["one", "two"]});
        let encoded = render(&value);
        assert!(encoded.starts_with("help[2]:"));
    }
}
