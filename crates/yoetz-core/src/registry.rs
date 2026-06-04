use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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

/// Output modality / task kind of a model. Used to keep non-chat models
/// (image generation, video, audio, embeddings, …) out of frontier chat picks.
/// An *unknown* kind (a serialized variant this build does not recognize) is
/// fail-open / chat-eligible so a new chat-like mode is never silently dropped.
/// An *unset* kind (no capability data at all — the common case) is instead
/// resolved structurally via [`ModelEntry::looks_like_chat_completion`], since
/// failing fully open there would let media/embedding models win frontier picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Chat,
    ImageGeneration,
    VideoGeneration,
    Audio,
    Embedding,
    Moderation,
    Rerank,
    /// A recognized non-chat mode that does not fit the categories above
    /// (litellm `search`, `ocr`, `vector_store`, …). Excluded from chat frontier.
    Other,
    /// Deserialize fallback for a serialized kind string this build does not
    /// recognize. Treated as chat-eligible (fail-open) so a future kind is
    /// never silently dropped from frontier.
    #[serde(other)]
    Unknown,
}

impl ModelKind {
    /// Whether a model of this kind can serve chat/multimodal completions and is
    /// therefore eligible to be a family frontier pick. Unknown kinds are
    /// eligible (fail-open) so a new chat-like mode is never silently dropped.
    pub fn is_chat_eligible(self) -> bool {
        matches!(self, ModelKind::Chat | ModelKind::Unknown)
    }

    /// Map a litellm `mode` string to a kind, covering the full authoritative
    /// litellm mode set. Returns `None` for unrecognized modes so ingest stays
    /// fail-open (the model remains chat-eligible).
    pub fn from_litellm_mode(mode: &str) -> Option<Self> {
        match mode.trim().to_ascii_lowercase().as_str() {
            "chat" | "completion" | "responses" => Some(ModelKind::Chat),
            "image_generation" | "image_edit" => Some(ModelKind::ImageGeneration),
            "video_generation" => Some(ModelKind::VideoGeneration),
            "audio_speech" | "audio_transcription" => Some(ModelKind::Audio),
            "embedding" => Some(ModelKind::Embedding),
            "moderation" => Some(ModelKind::Moderation),
            "rerank" => Some(ModelKind::Rerank),
            "search" | "ocr" | "vector_store" => Some(ModelKind::Other),
            _ => None,
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
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub kind: Option<ModelKind>,
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
        if other.kind.is_some() {
            self.kind = other.kind;
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

    /// Structural proxy for chat/completion eligibility, used when an explicit
    /// [`ModelKind`] is absent (the common case — most catalog entries, both
    /// live and embedded, do not carry a `kind`). A chat/completion model
    /// advertises a max output-token budget *or* charges for generated
    /// (completion) tokens. Media generators (imagen, veo) and search endpoints
    /// (`*_pse/search`) have neither, and — importantly — neither do embedding
    /// models: an embedding has input (`prompt_per_1k`) pricing but no output
    /// budget and `completion_per_1k == 0.0`, so input pricing alone is *not* a
    /// chat signal. Relying on `kind` alone is not enough: when it is unset,
    /// failing fully open would let a media or embedding model win a family's
    /// frontier pick on the version signal. This keeps a genuinely new chat
    /// model eligible (it has an output budget or output pricing) while
    /// excluding media/search/embedding models.
    pub fn looks_like_chat_completion(&self) -> bool {
        self.max_output_tokens.is_some() || self.pricing.completion_per_1k.is_some_and(|c| c > 0.0)
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

    pub fn prune_provider(&mut self, provider: &str, keep_ids: &HashSet<String>) {
        self.models.retain(|model| {
            model.provider.as_deref() != Some(provider) || keep_ids.contains(&model.id)
        });
        self.rebuild_index();
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
    /// Name patterns define Mini/Preview/explicit Flagship labels; pricing then
    /// promotes the family's most expensive non-reasoning Standard *or Preview*
    /// model to Flagship. Promoting Preview matters because a frontier model
    /// often ships under a `-preview` label while still being the family's
    /// flagship (e.g. `gemini-3-pro-preview`); without this it would lose the
    /// frontier tiebreak to a cheaper Standard model such as an open `gemma`.
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

            // Refine tiers by price: promote the family's most expensive
            // "serious" model to Flagship. "Serious" excludes Mini (a cheap
            // flash/image mini must not own the family's top price and block the
            // real flagship) and reasoning models (priced high for thinking, not
            // general capability). Name-Flagships stay in the set so they count
            // toward the top price but need no promotion. Preview is promotable
            // because a flagship frequently ships under a `-preview` label
            // (e.g. gemini-3-pro-preview); otherwise it loses the frontier
            // tiebreak to a cheaper Standard model such as an open `gemma`. We
            // never demote a model to Mini based on price.
            let serious_priced: Vec<(usize, f64)> = indices
                .iter()
                .filter_map(|&idx| {
                    let model = &self.models[idx];
                    let tier = model.tier.unwrap_or(ModelTier::Standard);
                    let serious = tier != ModelTier::Mini && !is_reasoning_model(&model.id);
                    serious
                        .then(|| model.pricing.completion_per_1k.map(|p| (idx, p)))
                        .flatten()
                })
                .collect();

            if serious_priced.len() >= 2 {
                let max_price = serious_priced
                    .iter()
                    .map(|&(_, p)| p)
                    .fold(f64::MIN, f64::max);
                let min_price = serious_priced
                    .iter()
                    .map(|&(_, p)| p)
                    .fold(f64::MAX, f64::min);

                // Only promote when price actually distinguishes a top model; if
                // every serious model costs the same we cannot single one out, so
                // leave the name-based tiers untouched. Promote only Standard or
                // Preview models at the top price — a flagship frequently ships
                // as `-preview`; a name-Flagship is already where it should be.
                if max_price > min_price {
                    for &(idx, price) in &serious_priced {
                        let tier = self.models[idx].tier.unwrap_or(ModelTier::Standard);
                        if price == max_price
                            && matches!(tier, ModelTier::Standard | ModelTier::Preview)
                        {
                            self.models[idx].tier = Some(ModelTier::Flagship);
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
            // Skip non-chat models (image generation, video, audio, embeddings, …):
            // a family frontier query must return the chat/multimodal flagship,
            // not a media generator. When a model carries an explicit kind we
            // trust it; when it does not (the common case — `kind` is almost
            // always unset in the live/embedded registry), fall back to a
            // structural proxy instead of failing fully open, otherwise an
            // image/video generator (e.g. imagen-4.0 > gemini-3 on version)
            // silently wins the family. A genuinely new chat model still passes
            // the proxy because it carries pricing or an output-token budget.
            let chat_eligible = match model.capability.as_ref().and_then(|cap| cap.kind) {
                Some(kind) => kind.is_chat_eligible(),
                None => model.looks_like_chat_completion(),
            };
            if !chat_eligible {
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
                    kind: None,
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
                    kind: None,
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

    #[test]
    fn prune_provider_removes_only_that_provider_outside_keep_set() {
        let mut reg = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "google/gemini-3-pro-preview".to_string(),
                    provider: Some("openrouter".to_string()),
                    ..Default::default()
                },
                ModelEntry {
                    id: "google/old-model".to_string(),
                    provider: Some("openrouter".to_string()),
                    ..Default::default()
                },
                ModelEntry {
                    id: "google/gemini-3.1-pro-preview".to_string(),
                    provider: Some("openrouter".to_string()),
                    ..Default::default()
                },
                ModelEntry {
                    id: "gemini/gemini-3-pro-preview".to_string(),
                    provider: Some("gemini".to_string()),
                    ..Default::default()
                },
                ModelEntry {
                    id: "local/custom".to_string(),
                    provider: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        reg.rebuild_index();

        let keep_ids = ["google/gemini-3.1-pro-preview".to_string()]
            .into_iter()
            .collect();
        reg.prune_provider("openrouter", &keep_ids);

        assert!(reg.find("google/gemini-3-pro-preview").is_none());
        assert!(reg.find("google/old-model").is_none());
        assert!(reg.find("google/gemini-3.1-pro-preview").is_some());
        assert!(reg.find("gemini/gemini-3-pro-preview").is_some());
        assert!(reg.find("local/custom").is_some());
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
        // because version is the primary signal — newer preview > older stable.
        // It is also the family's most expensive model, so it is promoted from
        // Preview to Flagship (a `-preview` model can be the family flagship).
        let google_frontier = frontier.iter().find(|e| e.family == "google").unwrap();
        assert_eq!(google_frontier.model.id, "google/gemini-3.1-pro-preview");
        assert_eq!(google_frontier.tier, ModelTier::Flagship);
    }

    #[test]
    fn frontier_excludes_image_generation_models() {
        // Reproduces the reported bug: an image-generation model
        // (imagen-4.0-ultra, version 4 beats gemini-3 on the primary version
        // signal) must NOT be returned as the gemini family frontier.
        let mut reg = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "google/gemini-3-pro-preview".to_string(),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.01),
                        ..Default::default()
                    },
                    capability: Some(ModelCapability {
                        kind: Some(ModelKind::Chat),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ModelEntry {
                    id: "google/imagen-4.0-ultra-generate-001".to_string(),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.04),
                        ..Default::default()
                    },
                    capability: Some(ModelCapability {
                        kind: Some(ModelKind::ImageGeneration),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        reg.rebuild_index();

        let frontier = reg.frontier();
        let google = frontier
            .iter()
            .find(|e| e.family == "google")
            .expect("google family has a chat frontier");
        assert_eq!(google.model.id, "google/gemini-3-pro-preview");
        assert!(
            !frontier.iter().any(|e| e.model.id.contains("imagen")),
            "image-generation model must never be a frontier pick"
        );
    }

    #[test]
    fn frontier_keeps_vision_chat_and_unknown_kind_models() {
        // A vision (image-INPUT) chat model stays eligible, and a model with no
        // kind at all is fail-open (still eligible).
        let mut reg = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "google/gemini-3.1-pro-preview".to_string(),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.02),
                        ..Default::default()
                    },
                    capability: Some(ModelCapability {
                        vision: Some(true),
                        kind: Some(ModelKind::Chat),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ModelEntry {
                    id: "anthropic/claude-opus-4-6".to_string(),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.075),
                        ..Default::default()
                    },
                    capability: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        reg.rebuild_index();

        let frontier = reg.frontier();
        assert_eq!(
            frontier
                .iter()
                .find(|e| e.family == "google")
                .unwrap()
                .model
                .id,
            "google/gemini-3.1-pro-preview"
        );
        assert_eq!(
            frontier
                .iter()
                .find(|e| e.family == "anthropic")
                .unwrap()
                .model
                .id,
            "anthropic/claude-opus-4-6"
        );
    }

    #[test]
    fn frontier_excludes_media_models_with_unset_kind() {
        // The real-world failure the explicit-kind test above does NOT catch:
        // the live/embedded registry does not populate `kind`, so a media model
        // arrives with `capability: None`. imagen-4.0 (version 4 > gemini-3 on
        // the primary version signal) and veo-3.1 (version 3.1 > 3) must still
        // be excluded so the gemini family frontier is the chat flagship.
        // Their shape mirrors the live registry: no kind, no token pricing, no
        // max output-token budget (imagen also has no context window; veo has a
        // tiny 1024 prompt cap).
        let mut reg = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "gemini/gemini-3-pro-preview".to_string(),
                    context_length: Some(1_048_576),
                    max_output_tokens: Some(65_535),
                    pricing: ModelPricing {
                        prompt_per_1k: Some(0.002),
                        completion_per_1k: Some(0.012),
                        ..Default::default()
                    },
                    capability: None,
                    ..Default::default()
                },
                ModelEntry {
                    id: "gemini/imagen-4.0-ultra-generate-001".to_string(),
                    capability: None,
                    ..Default::default()
                },
                ModelEntry {
                    id: "gemini/veo-3.1-generate-preview".to_string(),
                    context_length: Some(1024),
                    capability: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        reg.rebuild_index();

        let frontier = reg.frontier();
        let gemini = frontier
            .iter()
            .find(|e| e.family == "gemini")
            .expect("gemini family has a chat frontier");
        assert_eq!(gemini.model.id, "gemini/gemini-3-pro-preview");
        assert!(
            !frontier
                .iter()
                .any(|e| e.model.id.contains("imagen") || e.model.id.contains("veo")),
            "media models with unset kind must never be a frontier pick"
        );
    }

    #[test]
    fn frontier_excludes_embedding_model_with_unset_kind() {
        // Embeddings carry input (prompt) pricing but no output budget and
        // completion_per_1k == 0.0. With kind unset, input pricing alone must
        // NOT admit them — otherwise a future higher-version embedding would win
        // the family frontier over the real chat flagship. The embedding here is
        // given version 5 (> gemini-3) precisely to prove version can't rescue it.
        let mut reg = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "gemini/gemini-3-pro-preview".to_string(),
                    context_length: Some(1_048_576),
                    max_output_tokens: Some(65_535),
                    pricing: ModelPricing {
                        prompt_per_1k: Some(0.002),
                        completion_per_1k: Some(0.012),
                        ..Default::default()
                    },
                    capability: None,
                    ..Default::default()
                },
                ModelEntry {
                    id: "gemini/gemini-embedding-5".to_string(),
                    context_length: Some(2048),
                    max_output_tokens: None,
                    pricing: ModelPricing {
                        prompt_per_1k: Some(0.00015),
                        completion_per_1k: Some(0.0),
                        ..Default::default()
                    },
                    capability: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        reg.rebuild_index();

        let frontier = reg.frontier();
        let gemini = frontier
            .iter()
            .find(|e| e.family == "gemini")
            .expect("gemini family has a chat frontier");
        assert_eq!(gemini.model.id, "gemini/gemini-3-pro-preview");
        assert!(
            !frontier.iter().any(|e| e.model.id.contains("embedding")),
            "embedding model (input pricing only, no output budget) must not be a frontier pick"
        );
    }

    #[test]
    fn frontier_keeps_subscription_chat_model_without_token_pricing() {
        // A flat-rate/subscription chat model (e.g. github_copilot's gemini-3)
        // carries no per-token pricing but does advertise a max output-token
        // budget. The structural proxy must keep it eligible so the fix does
        // not silently drop real chat models alongside the media generators.
        let mut reg = ModelRegistry {
            models: vec![ModelEntry {
                id: "github_copilot/gemini-3-pro-preview".to_string(),
                context_length: Some(128_000),
                max_output_tokens: Some(64_000),
                pricing: ModelPricing::default(),
                capability: None,
                ..Default::default()
            }],
            ..Default::default()
        };
        reg.rebuild_index();

        let frontier = reg.frontier();
        assert_eq!(
            frontier
                .iter()
                .find(|e| e.family == "github_copilot")
                .expect("subscription chat model stays a frontier pick")
                .model
                .id,
            "github_copilot/gemini-3-pro-preview"
        );
    }

    #[test]
    fn frontier_prefers_priced_preview_flagship_over_cheaper_standard() {
        // A family's flagship frequently ships under a `-preview` label
        // (gemini-3-pro-preview) while a cheaper open Standard model (gemma)
        // ties on the version signal. The priciest non-reasoning model must be
        // recognized as the flagship so the real Gemini flagship wins the
        // frontier pick, not the small open model. Without the Preview→Flagship
        // promotion this returns gemma (Standard outranks Preview in the
        // tiebreak), which is the bug surfaced once media models are excluded.
        let mut reg = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "gemini/gemini-3-pro-preview".to_string(),
                    context_length: Some(1_048_576),
                    max_output_tokens: Some(65_535),
                    pricing: ModelPricing {
                        prompt_per_1k: Some(0.002),
                        completion_per_1k: Some(0.012),
                        ..Default::default()
                    },
                    capability: None,
                    ..Default::default()
                },
                ModelEntry {
                    id: "gemini/gemma-3-27b-it".to_string(),
                    context_length: Some(131_072),
                    max_output_tokens: Some(8192),
                    pricing: ModelPricing {
                        prompt_per_1k: Some(0.0),
                        completion_per_1k: Some(0.0),
                        ..Default::default()
                    },
                    capability: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        reg.rebuild_index();

        let frontier = reg.frontier();
        assert_eq!(
            frontier
                .iter()
                .find(|e| e.family == "gemini")
                .expect("gemini family has a chat frontier")
                .model
                .id,
            "gemini/gemini-3-pro-preview"
        );
    }

    #[test]
    fn model_kind_from_litellm_mode_maps_known_and_unknown() {
        // Chat-eligible modes.
        for m in ["chat", "completion", "responses"] {
            assert_eq!(
                ModelKind::from_litellm_mode(m),
                Some(ModelKind::Chat),
                "{m}"
            );
        }
        // Non-chat modes from the authoritative litellm set.
        assert_eq!(
            ModelKind::from_litellm_mode("image_generation"),
            Some(ModelKind::ImageGeneration)
        );
        assert_eq!(
            ModelKind::from_litellm_mode("image_edit"),
            Some(ModelKind::ImageGeneration)
        );
        assert_eq!(
            ModelKind::from_litellm_mode("video_generation"),
            Some(ModelKind::VideoGeneration)
        );
        assert_eq!(
            ModelKind::from_litellm_mode("audio_transcription"),
            Some(ModelKind::Audio)
        );
        assert_eq!(
            ModelKind::from_litellm_mode("embedding"),
            Some(ModelKind::Embedding)
        );
        assert_eq!(
            ModelKind::from_litellm_mode("rerank"),
            Some(ModelKind::Rerank)
        );
        for m in ["search", "ocr", "vector_store"] {
            assert_eq!(
                ModelKind::from_litellm_mode(m),
                Some(ModelKind::Other),
                "{m}"
            );
        }
        // Unknown / new modes stay fail-open (None -> chat-eligible).
        assert_eq!(ModelKind::from_litellm_mode("brand_new_mode"), None);

        // Eligibility: only Chat (and the Unknown deserialize fallback) is a
        // valid frontier pick; every recognized non-chat kind is excluded.
        assert!(ModelKind::Chat.is_chat_eligible());
        assert!(ModelKind::Unknown.is_chat_eligible());
        for k in [
            ModelKind::ImageGeneration,
            ModelKind::VideoGeneration,
            ModelKind::Audio,
            ModelKind::Embedding,
            ModelKind::Moderation,
            ModelKind::Rerank,
            ModelKind::Other,
        ] {
            assert!(!k.is_chat_eligible(), "{k:?} must not be chat-eligible");
        }
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
