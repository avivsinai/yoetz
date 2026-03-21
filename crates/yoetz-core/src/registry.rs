use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    Mini,
    Preview,
    Standard,
    Flagship,
}

impl std::fmt::Display for ModelTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelTier::Flagship => write!(f, "flagship"),
            ModelTier::Standard => write!(f, "standard"),
            ModelTier::Mini => write!(f, "mini"),
            ModelTier::Preview => write!(f, "preview"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontierEntry {
    pub family: String,
    pub model: ModelEntry,
    pub tier: ModelTier,
}

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
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tier: Option<ModelTier>,
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
        if other.tier.is_some() {
            self.tier = other.tier;
        }
    }

    /// Extract the provider family from the model ID (first segment before `/`).
    pub fn family(&self) -> &str {
        self.id.split('/').next().unwrap_or(&self.id)
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

    pub fn with_inferred_tiers(mut self) -> Self {
        self.infer_tiers();
        self
    }

    /// Infer tier for each model based on pricing and name patterns.
    /// Name patterns define Mini/Preview/explicit Flagship labels; pricing only
    /// promotes Standard models to Flagship within a family.
    pub fn infer_tiers(&mut self) {
        // Group models by family
        let mut families: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, model) in self.models.iter().enumerate() {
            let family = model.family().to_string();
            families.entry(family).or_default().push(idx);
        }

        for indices in families.values() {
            if indices.is_empty() {
                continue;
            }

            // Classify each model by name pattern first
            for &idx in indices {
                let model = &self.models[idx];
                let name_lower = model.id.to_lowercase();
                let tier = infer_tier_from_name(&name_lower);
                self.models[idx].tier = Some(tier);
            }

            // If we have pricing data, use it to refine:
            // within the family, the most expensive Standard model can be promoted
            // to Flagship. We never demote a model to Mini based on relative price.
            let mut priced: Vec<(usize, f64)> = indices
                .iter()
                .filter_map(|&idx| self.models[idx].pricing.completion_per_1k.map(|p| (idx, p)))
                .collect();

            if priced.len() >= 2 {
                priced.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

                // Exclude reasoning models from price-based promotion (they're expensive
                // due to thinking tokens, not because they're general-purpose flagships)
                let non_reasoning_priced: Vec<(usize, f64)> = priced
                    .iter()
                    .filter(|&&(idx, _)| !is_reasoning_model(&self.models[idx].id))
                    .copied()
                    .collect();

                if non_reasoning_priced.len() >= 2 {
                    let max_price = non_reasoning_priced[0].1;
                    let min_price = non_reasoning_priced.last().unwrap().1;

                    if max_price > min_price {
                        for &(idx, price) in &non_reasoning_priced {
                            let name_tier = self.models[idx].tier.unwrap_or(ModelTier::Standard);
                            if name_tier == ModelTier::Standard && price == max_price {
                                self.models[idx].tier = Some(ModelTier::Flagship);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Return the frontier model per provider family.
    /// Tiers are inferred internally so callers do not depend on call order.
    /// Only considers properly namespaced models (`provider/model` format).
    pub fn frontier(&self) -> Vec<FrontierEntry> {
        let registry = self.clone().with_inferred_tiers();
        let mut best: HashMap<String, FrontierEntry> = HashMap::new();

        for model in &registry.models {
            // Skip models without provider/model format (litellm duplicates)
            if !model.id.contains('/') {
                continue;
            }
            let tier = match model.tier {
                Some(t) => t,
                None => continue,
            };
            // Skip mini-tier models — they're explicitly small/cheap, not frontier picks.
            if tier == ModelTier::Mini {
                continue;
            }
            let family = model.family().to_string();
            let dominated = best.get(&family).is_some_and(|existing| {
                let existing_ver = extract_version(&existing.model.id);
                let new_ver = extract_version(&model.id);

                // Primary: higher version number wins (newer model is the frontier pick)
                if existing_ver != new_ver {
                    return existing_ver >= new_ver;
                }
                // Same version: higher tier wins (flagship > preview of same version)
                if existing.tier != tier {
                    return existing.tier > tier;
                }
                // Same version + tier: higher context length wins
                let existing_ctx = existing.model.context_length.unwrap_or(0);
                let new_ctx = model.context_length.unwrap_or(0);
                if existing_ctx != new_ctx {
                    return existing_ctx >= new_ctx;
                }
                // Same everything: shorter name wins (base model over specialized variant)
                existing.model.id.len() <= model.id.len()
            });
            if !dominated {
                best.insert(
                    family.clone(),
                    FrontierEntry {
                        family,
                        model: model.clone(),
                        tier,
                    },
                );
            }
        }

        let mut entries: Vec<FrontierEntry> = best.into_values().collect();
        entries.sort_by(|a, b| a.family.cmp(&b.family));
        entries
    }
}

/// Extract version numbers from a model ID for comparison.
/// E.g. "anthropic/claude-opus-4.6" → [4, 6], "openai/gpt-5.4-pro" → [5, 4]
/// Only extracts from pure numeric tokens and dotted versions.
/// Skips date suffixes (4+ digit numbers) and parameter counts (e.g. "70b").
fn extract_version(id: &str) -> Vec<u32> {
    let name_part = id.rsplit('/').next().unwrap_or(id);
    let name_part = name_part.split(':').next().unwrap_or(name_part);
    let mut versions = Vec::new();
    for token in name_part.split(['-', '_']) {
        // Stop at date-like tokens (4+ digit pure numbers like 2024, 0528)
        if token.len() >= 4 && token.chars().all(|c| c.is_ascii_digit()) {
            break;
        }
        // Strip leading 'v' prefix (e.g. "v3.2" → "3.2")
        let token = token.strip_prefix('v').unwrap_or(token);
        // Dotted version like "5.4" or "3.1"
        if token.contains('.') {
            for part in token.split('.') {
                if let Ok(n) = part.parse::<u32>() {
                    versions.push(n);
                }
            }
        } else if let Ok(n) = token.parse::<u32>() {
            // Pure number token (e.g. "4" in "grok-4")
            if n < 100 {
                versions.push(n);
            }
        }
        // Skip mixed tokens like "4o", "20b", "70b" — not version numbers
    }
    versions
}

/// Check if a model is a reasoning-specific model (expensive due to thinking tokens).
fn is_reasoning_model(id: &str) -> bool {
    let name_part = id.to_lowercase();
    let name_part = name_part.rsplit('/').next().unwrap_or(&name_part);
    let tokens: Vec<&str> = name_part.split(['-', '.', '_', ':']).collect();
    tokens
        .iter()
        .any(|t| matches!(*t, "o1" | "o3" | "o4" | "r1" | "r1t2"))
}

/// Infer tier from name patterns. Returns Standard when no clear signal.
/// Mini signals take priority over preview (a flash-preview is still Mini).
fn infer_tier_from_name(name_lower: &str) -> ModelTier {
    // Extract the part after the last `/` for pattern matching
    let name_part = name_lower.rsplit('/').next().unwrap_or(name_lower);
    // Strip version suffixes like :free, :extended
    let name_part = name_part.split(':').next().unwrap_or(name_part);

    // Tokenize on `-` and `.` for word-boundary matching
    let tokens: Vec<&str> = name_part.split(['-', '.', '_']).collect();

    // Mini signals first — a flash-preview or haiku-beta is still Mini
    if tokens
        .iter()
        .any(|t| matches!(*t, "mini" | "flash" | "nano" | "lite" | "haiku" | "instant"))
    {
        return ModelTier::Mini;
    }

    // Preview signals (after mini check)
    if tokens
        .iter()
        .any(|t| matches!(*t, "preview" | "beta" | "exp"))
    {
        return ModelTier::Preview;
    }

    // Reasoning models — expensive due to thinking tokens, not general flagship
    if tokens
        .iter()
        .any(|t| matches!(*t, "o1" | "o3" | "o4" | "r1" | "r1t2"))
    {
        return ModelTier::Standard;
    }

    // Flagship signals
    if tokens
        .iter()
        .any(|t| matches!(*t, "opus" | "ultra" | "heavy" | "pro"))
    {
        return ModelTier::Flagship;
    }

    ModelTier::Standard
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
                tier: None,
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
                tier: None,
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

    fn multi_provider_registry() -> ModelRegistry {
        let mut reg = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "openai/gpt-5.4".to_string(),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.060),
                        ..Default::default()
                    },
                    context_length: Some(128_000),
                    ..Default::default()
                },
                ModelEntry {
                    id: "openai/gpt-5.4-mini".to_string(),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.002),
                        ..Default::default()
                    },
                    context_length: Some(128_000),
                    ..Default::default()
                },
                ModelEntry {
                    id: "openai/gpt-5.3".to_string(),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.030),
                        ..Default::default()
                    },
                    context_length: Some(128_000),
                    ..Default::default()
                },
                ModelEntry {
                    id: "anthropic/claude-opus-4-6".to_string(),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.075),
                        ..Default::default()
                    },
                    context_length: Some(1_000_000),
                    ..Default::default()
                },
                ModelEntry {
                    id: "anthropic/claude-sonnet-4-6".to_string(),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.015),
                        ..Default::default()
                    },
                    context_length: Some(200_000),
                    ..Default::default()
                },
                ModelEntry {
                    id: "anthropic/claude-haiku-4-5".to_string(),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.005),
                        ..Default::default()
                    },
                    context_length: Some(200_000),
                    ..Default::default()
                },
                ModelEntry {
                    id: "google/gemini-2.5-pro".to_string(),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.010),
                        ..Default::default()
                    },
                    context_length: Some(1_000_000),
                    ..Default::default()
                },
                ModelEntry {
                    id: "google/gemini-2.5-flash".to_string(),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.001),
                        ..Default::default()
                    },
                    context_length: Some(1_000_000),
                    ..Default::default()
                },
                ModelEntry {
                    id: "google/gemini-3.1-pro-preview".to_string(),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.012),
                        ..Default::default()
                    },
                    context_length: Some(1_000_000),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        reg.rebuild_index();
        reg
    }

    #[test]
    fn infer_tiers_name_patterns() {
        assert_eq!(infer_tier_from_name("openai/gpt-5.4"), ModelTier::Standard);
        assert_eq!(infer_tier_from_name("openai/gpt-5.4-mini"), ModelTier::Mini);
        assert_eq!(
            infer_tier_from_name("anthropic/claude-opus-4-6"),
            ModelTier::Flagship
        );
        assert_eq!(
            infer_tier_from_name("anthropic/claude-haiku-4-5"),
            ModelTier::Mini
        );
        assert_eq!(
            infer_tier_from_name("google/gemini-3.1-flash"),
            ModelTier::Mini
        );
        assert_eq!(
            infer_tier_from_name("google/gemini-3.1-pro"),
            ModelTier::Flagship
        );
        assert_eq!(
            infer_tier_from_name("google/gemini-3.2-pro-preview"),
            ModelTier::Preview
        );
        // flash-lite-preview is Mini (mini signals take priority over preview)
        assert_eq!(
            infer_tier_from_name("google/gemini-3.1-flash-lite-preview"),
            ModelTier::Mini
        );
    }

    #[test]
    fn infer_tiers_price_promotes_flagship_only() {
        let mut reg = multi_provider_registry();
        reg.infer_tiers();

        // gpt-5.4 is Standard by name but most expensive in openai family → Flagship
        let gpt54 = reg.find("openai/gpt-5.4").unwrap();
        assert_eq!(gpt54.tier, Some(ModelTier::Flagship));

        // gpt-5.4-mini is Mini by name — price doesn't override
        let gpt54_mini = reg.find("openai/gpt-5.4-mini").unwrap();
        assert_eq!(gpt54_mini.tier, Some(ModelTier::Mini));

        // gpt-5.3: mid-price, no name signal → stays Standard.
        let gpt53 = reg.find("openai/gpt-5.3").unwrap();
        assert_eq!(gpt53.tier, Some(ModelTier::Standard));

        // opus is Flagship by name
        let opus = reg.find("anthropic/claude-opus-4-6").unwrap();
        assert_eq!(opus.tier, Some(ModelTier::Flagship));

        // haiku is Mini by name
        let haiku = reg.find("anthropic/claude-haiku-4-5").unwrap();
        assert_eq!(haiku.tier, Some(ModelTier::Mini));
    }

    #[test]
    fn frontier_returns_one_per_family() {
        let reg = multi_provider_registry();
        let frontier = reg.frontier();

        let families: Vec<&str> = frontier.iter().map(|e| e.family.as_str()).collect();
        // Should have openai, anthropic, google
        assert!(families.contains(&"openai"));
        assert!(families.contains(&"anthropic"));
        assert!(families.contains(&"google"));

        // Each family appears exactly once
        assert_eq!(
            families.len(),
            families
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
        );

        // OpenAI frontier should be gpt-5.4 (Flagship by price)
        let openai_frontier = frontier.iter().find(|e| e.family == "openai").unwrap();
        assert_eq!(openai_frontier.model.id, "openai/gpt-5.4");
        assert_eq!(openai_frontier.tier, ModelTier::Flagship);

        // Anthropic frontier should be opus (Flagship by name)
        let anthropic_frontier = frontier.iter().find(|e| e.family == "anthropic").unwrap();
        assert_eq!(anthropic_frontier.model.id, "anthropic/claude-opus-4-6");
        assert_eq!(anthropic_frontier.tier, ModelTier::Flagship);

        // Google frontier: gemini-3.1-pro-preview (version 3.1) beats gemini-2.5-pro (version 2.5)
        // because version is the primary signal — newer preview > older stable
        let google_frontier = frontier.iter().find(|e| e.family == "google").unwrap();
        assert_eq!(google_frontier.model.id, "google/gemini-3.1-pro-preview");
        assert_eq!(google_frontier.tier, ModelTier::Preview);
    }

    #[test]
    fn two_model_family_does_not_infer_false_mini() {
        let mut reg = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "anthropic/claude-opus-4-6".to_string(),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.075),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ModelEntry {
                    id: "anthropic/claude-sonnet-4-6".to_string(),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.015),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        reg.rebuild_index();
        reg.infer_tiers();

        assert_eq!(
            reg.find("anthropic/claude-opus-4-6").and_then(|m| m.tier),
            Some(ModelTier::Flagship)
        );
        assert_eq!(
            reg.find("anthropic/claude-sonnet-4-6").and_then(|m| m.tier),
            Some(ModelTier::Standard)
        );
    }

    #[test]
    fn frontier_is_self_contained_without_prior_infer_tiers_call() {
        let reg = multi_provider_registry();
        let frontier = reg.frontier();

        let openai = frontier.iter().find(|e| e.family == "openai").unwrap();
        assert_eq!(openai.model.id, "openai/gpt-5.4");
        assert_eq!(openai.tier, ModelTier::Flagship);
    }

    #[test]
    fn tier_serialization_roundtrip() {
        let entry = ModelEntry {
            id: "test/model".to_string(),
            tier: Some(ModelTier::Flagship),
            ..Default::default()
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"tier\":\"flagship\""));

        let roundtrip: ModelEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.tier, Some(ModelTier::Flagship));

        // None tier should not appear in JSON
        let entry_no_tier = ModelEntry {
            id: "test/model2".to_string(),
            ..Default::default()
        };
        let json2 = serde_json::to_string(&entry_no_tier).unwrap();
        assert!(!json2.contains("tier"));

        // Deserializing old JSON without tier field should work
        let old_json = r#"{"id":"test/old","pricing":{}}"#;
        let old_entry: ModelEntry = serde_json::from_str(old_json).unwrap();
        assert_eq!(old_entry.tier, None);
    }

    #[test]
    fn extract_version_dotted() {
        assert_eq!(extract_version("openai/gpt-5.4-pro"), vec![5, 4]);
        assert_eq!(extract_version("anthropic/claude-opus-4.6"), vec![4, 6]);
        assert_eq!(extract_version("google/gemini-3.1-pro-preview"), vec![3, 1]);
        assert_eq!(extract_version("x-ai/grok-4.20-beta"), vec![4, 20]);
    }

    #[test]
    fn extract_version_bare_numbers() {
        assert_eq!(extract_version("x-ai/grok-4"), vec![4]);
        assert_eq!(extract_version("anthropic/claude-opus-4-6"), vec![4, 6]);
    }

    #[test]
    fn extract_version_v_prefix() {
        assert_eq!(
            extract_version("deepseek/deepseek-v3.2-speciale"),
            vec![3, 2]
        );
        assert_eq!(extract_version("deepseek/deepseek-v3-chat"), vec![3]);
    }

    #[test]
    fn extract_version_stops_at_dates() {
        // Date suffix should not contribute to version
        assert_eq!(
            extract_version("openai/gpt-4o-2024-11-20"),
            Vec::<u32>::new()
        );
        assert_eq!(extract_version("deepseek/deepseek-chat-v3-0324"), vec![3]);
        assert_eq!(
            extract_version("mistralai/mistral-large-2411"),
            Vec::<u32>::new()
        );
    }

    #[test]
    fn extract_version_skips_param_counts() {
        // "70b", "20b", "27b", "405b" are parameter counts, not versions
        assert_eq!(extract_version("meta-llama/llama-3.1-405b"), vec![3, 1]);
        assert_eq!(
            extract_version("deepseek/deepseek-r1-distill-llama-70b"),
            Vec::<u32>::new() // "r1" is a series name, not a version
        );
        assert_eq!(extract_version("google/gemma-3-27b-it"), vec![3]);
    }

    #[test]
    fn extract_version_empty() {
        assert_eq!(extract_version("openai/gpt-oss"), Vec::<u32>::new());
        assert_eq!(extract_version("mancer/weaver"), Vec::<u32>::new());
    }
}
