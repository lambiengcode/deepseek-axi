use crate::error::CliError;
use serde::Serialize;

pub const MODEL_FLASH: &str = "deepseek-v4-flash";
pub const MODEL_PRO: &str = "deepseek-v4-pro";

#[derive(Serialize)]
pub struct ModelInfo {
    pub model: &'static str,
    pub params: &'static str,
    pub context: &'static str,
    #[serde(rename = "outputPriceHint")]
    pub output_price_hint: &'static str,
    #[serde(rename = "whenToUse")]
    pub when_to_use: &'static str,
}

pub fn model_info() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            model: MODEL_FLASH,
            params: "284B total / 13B active",
            context: "1M",
            output_price_hint: "~32% of pro",
            when_to_use: "default: chat, streaming, repeated tool steps",
        },
        ModelInfo {
            model: MODEL_PRO,
            params: "1.6T total / 49B active",
            context: "1M",
            output_price_hint: "baseline",
            when_to_use: "hard reasoning, review-heavy work",
        },
    ]
}

pub fn resolve_model(flag: Option<&str>) -> Result<String, CliError> {
    match flag {
        None => Ok(MODEL_FLASH.to_string()),
        Some("flash") => Ok(MODEL_FLASH.to_string()),
        Some("pro") => Ok(MODEL_PRO.to_string()),
        Some(other) => Err(CliError::validation(
            format!("Unknown --model value: {other}"),
            vec!["Use --model flash or --model pro".into()],
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_info_lists_both_v4_models() {
        let info = model_info();
        assert_eq!(info.len(), 2);
        assert_eq!(info[0].model, MODEL_FLASH);
        assert_eq!(info[1].model, MODEL_PRO);
    }

    #[test]
    fn resolve_model_defaults_to_flash() {
        assert_eq!(resolve_model(None).unwrap(), MODEL_FLASH);
    }

    #[test]
    fn resolve_model_accepts_flash_and_pro() {
        assert_eq!(resolve_model(Some("flash")).unwrap(), MODEL_FLASH);
        assert_eq!(resolve_model(Some("pro")).unwrap(), MODEL_PRO);
    }

    #[test]
    fn resolve_model_rejects_unknown_values() {
        let err = resolve_model(Some("opus")).unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
    }
}
