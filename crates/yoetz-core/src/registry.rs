use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelPricing {
    pub prompt_per_1k: Option<f64>,
    pub completion_per_1k: Option<f64>,
    pub request: Option<f64>,
}

impl ModelPricing {
    pub fn estimate(&self, input_tokens: usize, output_tokens: usize) -> Option<f64> {
        let prompt_cost = self
            .prompt_per_1k
            .map(|p| p * input_tokens as f64 / 1000.0)?;
        let completion_cost = self
            .completion_per_1k
            .map(|c| c * output_tokens as f64 / 1000.0)?;
        let request_cost = self.request.unwrap_or(0.0);
        Some(prompt_cost + completion_cost + request_cost)
    }

    fn merge_from(&mut self, other: ModelPricing) {
        if other.prompt_per_1k.is_some() {
            self.prompt_per_1k = other.prompt_per_1k;
        }
        if other.completion_per_1k.is_some() {
            self.completion_per_1k = other.completion_per_1k;
        }
        if other.request.is_some() {
            self.request = other.request;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelEntry {
    pub id: String,
    pub context_length: Option<usize>,
    pub max_output_tokens: Option<usize>,
    pub pricing: ModelPricing,
    pub provider: Option<String>,
    pub capability: Option<ModelCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelCapability {
    pub vision: Option<bool>,
    pub reasoning: Option<bool>,
    pub web_search: Option<bool>,
}

impl ModelCapability {
    fn merge_from(&mut self, other: ModelCapability) {
        if other.vision.is_some() {
            self.vision = other.vision;
        }
        if other.reasoning.is_some() {
            self.reasoning = other.reasoning;
        }
        if other.web_search.is_some() {
            self.web_search = other.web_search;
        }
    }
}

impl ModelEntry {
    fn merge_from(&mut self, other: ModelEntry) {
        debug_assert_eq!(self.id, other.id);
        if other.context_length.is_some() {
            self.context_length = other.context_length;
        }
        if other.max_output_tokens.is_some() {
            self.max_output_tokens = other.max_output_tokens;
        }
        self.pricing.merge_from(other.pricing);
        if other.provider.is_some() {
            self.provider = other.provider;
        }
        match (&mut self.capability, other.capability) {
            (Some(existing), Some(other)) => existing.merge_from(other),
            (None, Some(other)) => self.capability = Some(other),
            _ => {}
        }
    }
}

/// In-memory model registry with pricing and capability data.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelRegistry {
    pub version: u32,
    pub updated_at: Option<String>,
    pub models: Vec<ModelEntry>,
    #[serde(skip, default)]
    index: HashMap<String, usize>,
}

impl ModelRegistry {
    pub fn find(&self, id: &str) -> Option<&ModelEntry> {
        if let Some(idx) = self.index.get(id) {
            return self.models.get(*idx);
        }
        self.models.iter().find(|m| m.id == id)
    }

    pub fn merge(&mut self, other: ModelRegistry) {
        if self.index.is_empty() && !self.models.is_empty() {
            self.rebuild_index();
        }
        for m in other.models {
            if let Some(idx) = self.index.get(&m.id).copied() {
                if let Some(existing) = self.models.get_mut(idx) {
                    existing.merge_from(m);
                }
            } else {
                self.models.push(m);
                let idx = self.models.len() - 1;
                self.index.insert(self.models[idx].id.clone(), idx);
            }
        }
    }

    pub fn rebuild_index(&mut self) {
        self.index.clear();
        for (idx, model) in self.models.iter().enumerate() {
            self.index.insert(model.id.clone(), idx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_preserves_existing_fields_when_new_entry_is_partial() {
        let mut base = ModelRegistry {
            models: vec![ModelEntry {
                id: "openai/gpt-5".to_string(),
                context_length: Some(128_000),
                max_output_tokens: Some(16_384),
                pricing: ModelPricing {
                    prompt_per_1k: Some(0.01),
                    completion_per_1k: Some(0.02),
                    request: None,
                },
                provider: Some("openrouter".to_string()),
                capability: Some(ModelCapability {
                    vision: Some(true),
                    reasoning: None,
                    web_search: Some(false),
                }),
            }],
            ..Default::default()
        };
        base.rebuild_index();

        let mut update = ModelRegistry {
            models: vec![ModelEntry {
                id: "openai/gpt-5".to_string(),
                context_length: None,
                max_output_tokens: Some(8_192),
                pricing: ModelPricing {
                    prompt_per_1k: None,
                    completion_per_1k: None,
                    request: Some(0.1),
                },
                provider: None,
                capability: Some(ModelCapability {
                    vision: None,
                    reasoning: Some(true),
                    web_search: None,
                }),
            }],
            ..Default::default()
        };
        update.rebuild_index();

        base.merge(update);

        let entry = base.find("openai/gpt-5").unwrap();
        assert_eq!(entry.context_length, Some(128_000));
        assert_eq!(entry.max_output_tokens, Some(8_192));
        assert_eq!(entry.pricing.prompt_per_1k, Some(0.01));
        assert_eq!(entry.pricing.completion_per_1k, Some(0.02));
        assert_eq!(entry.pricing.request, Some(0.1));
        assert_eq!(entry.provider.as_deref(), Some("openrouter"));

        let capability = entry.capability.as_ref().unwrap();
        assert_eq!(capability.vision, Some(true));
        assert_eq!(capability.reasoning, Some(true));
        assert_eq!(capability.web_search, Some(false));
    }
}
