use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelPricing {
    pub prompt_per_1k: Option<f64>,
    pub completion_per_1k: Option<f64>,
    pub request: Option<f64>,
}

impl ModelPricing {
    pub fn estimate(&self, input_tokens: usize, output_tokens: usize) -> Option<f64> {
        let prompt_cost = self.prompt_per_1k.map(|p| p * input_tokens as f64 / 1000.0)?;
        let completion_cost = self
            .completion_per_1k
            .map(|c| c * output_tokens as f64 / 1000.0)?;
        let request_cost = self.request.unwrap_or(0.0);
        Some(prompt_cost + completion_cost + request_cost)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelEntry {
    pub id: String,
    pub context_length: Option<usize>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelRegistry {
    pub version: u32,
    pub updated_at: Option<String>,
    pub models: Vec<ModelEntry>,
}

impl ModelRegistry {
    pub fn find(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id)
    }

    pub fn merge(&mut self, other: ModelRegistry) {
        for m in other.models {
            if let Some(existing) = self.models.iter_mut().find(|x| x.id == m.id) {
                *existing = m;
            } else {
                self.models.push(m);
            }
        }
    }
}
