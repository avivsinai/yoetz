use serde_json::Value;
use std::collections::HashMap;

use crate::error::{LiteLLMError, Result};

#[derive(Debug, Clone)]
pub struct ModelPricing {
    pub input_cost_per_1k: Option<f64>,
    pub output_cost_per_1k: Option<f64>,
    pub max_input_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub mode: Option<String>,
    pub provider: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Registry {
    pub models: HashMap<String, ModelPricing>,
}

impl Registry {
    pub fn load_embedded() -> Result<Self> {
        let raw = include_str!("../data/model_prices_and_context_window.json");
        let json: Value = serde_json::from_str(raw)
            .map_err(|e| LiteLLMError::Parse(format!("model registry: {e}")))?;
        let mut models = HashMap::new();
        let map = json
            .as_object()
            .ok_or_else(|| LiteLLMError::Parse("model registry not an object".into()))?;
        for (name, entry) in map {
            if name == "sample_spec" {
                continue;
            }
            if let Some(obj) = entry.as_object() {
                let input = obj
                    .get("input_cost_per_token")
                    .and_then(|v| v.as_f64())
                    .map(|v| v * 1000.0);
                let output = obj
                    .get("output_cost_per_token")
                    .and_then(|v| v.as_f64())
                    .map(|v| v * 1000.0);
                let max_input = obj
                    .get("max_input_tokens")
                    .or_else(|| obj.get("max_tokens"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let max_output = obj
                    .get("max_output_tokens")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let mode = obj
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let provider = obj
                    .get("litellm_provider")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                models.insert(
                    name.to_string(),
                    ModelPricing {
                        input_cost_per_1k: input,
                        output_cost_per_1k: output,
                        max_input_tokens: max_input,
                        max_output_tokens: max_output,
                        mode,
                        provider,
                    },
                );
            }
        }
        Ok(Self { models })
    }

    pub fn get(&self, model: &str) -> Option<&ModelPricing> {
        self.models.get(model)
    }

    pub fn estimate_cost(&self, model: &str, input_tokens: u32, output_tokens: u32) -> Option<f64> {
        let pricing = self.models.get(model)?;
        let input = pricing
            .input_cost_per_1k
            .map(|v| v * input_tokens as f64 / 1000.0)?;
        let output = pricing
            .output_cost_per_1k
            .map(|v| v * output_tokens as f64 / 1000.0)?;
        Some(input + output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_registry() {
        let registry = Registry::load_embedded().unwrap();
        assert!(!registry.models.is_empty());
    }
}
