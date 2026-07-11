use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
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
    /// Policy-ranked moving pointer when its product line differs from `model`.
    /// Observability only; aliases never participate in frontier selection.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub alias_disagreement: Option<String>,
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
    /// Upstream catalog creation time as a Unix timestamp, when available.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub created: Option<u64>,
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
        if other.created.is_some() {
            self.created = other.created;
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
        let mut best_by_line: HashMap<(String, String), RankedFrontierEntry> = HashMap::new();
        let mut best_alias: HashMap<String, RankedFrontierEntry> = HashMap::new();
        let mut alias_candidates: HashMap<String, Vec<RankedFrontierEntry>> = HashMap::new();
        let mut concrete_lines: HashMap<String, HashSet<String>> = HashMap::new();

        for model in &registry.models {
            let Some(parsed) = ParsedModelId::parse(&model.id) else {
                continue;
            };
            if !parsed.alias && !parsed.product_line.is_empty() {
                concrete_lines
                    .entry(parsed.family.clone())
                    .or_default()
                    .insert(parsed.product_line.clone());
            }
            let tier = match model.tier {
                Some(t) => t,
                None => continue,
            };
            // `~vendor/*-latest` entries are moving pointers, not concrete
            // models. They remain resolvable through normal lookup, but a
            // frontier listing must return an inspectable model ID.
            if parsed.alias {
                if tier != ModelTier::Mini {
                    let candidate = RankedFrontierEntry {
                        entry: FrontierEntry {
                            family: parsed.family.clone(),
                            model: model.clone(),
                            tier,
                            alias_disagreement: None,
                        },
                        parsed,
                    };
                    alias_candidates
                        .entry(candidate.entry.family.clone())
                        .or_default()
                        .push(candidate.clone());
                    let dominates =
                        best_alias
                            .get(&candidate.entry.family)
                            .is_none_or(|existing| {
                                across_product_lines_is_better(&candidate.entry, &existing.entry)
                            });
                    if dominates {
                        best_alias.insert(candidate.entry.family.clone(), candidate);
                    }
                }
                continue;
            }
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

            let line_key = (parsed.family.clone(), parsed.product_line.clone());
            let dominates = best_by_line.get(&line_key).is_none_or(|existing| {
                within_product_line_is_better(
                    model,
                    tier,
                    &parsed,
                    &existing.entry.model,
                    existing.entry.tier,
                    &existing.parsed,
                )
            });
            if dominates {
                best_by_line.insert(
                    line_key,
                    RankedFrontierEntry {
                        entry: FrontierEntry {
                            family: parsed.family.clone(),
                            model: model.clone(),
                            tier,
                            alias_disagreement: None,
                        },
                        parsed,
                    },
                );
            }
        }

        let mut best: HashMap<String, FrontierEntry> = HashMap::new();
        for candidate in best_by_line.into_values() {
            let dominates = best
                .get(&candidate.entry.family)
                .is_none_or(|existing| across_product_lines_is_better(&candidate.entry, existing));
            if dominates {
                best.insert(candidate.entry.family.clone(), candidate.entry);
            }
        }

        let mut entries: Vec<FrontierEntry> = best
            .into_values()
            .map(|mut entry| {
                let selected_line = ParsedModelId::parse(&entry.model.id)
                    .map(|parsed| parsed.product_line)
                    .unwrap_or_default();
                if let Some(alias) = best_alias.get(&entry.family) {
                    let alias_disagrees = alias_line_definitely_disagrees(
                        &alias.parsed.product_line,
                        &selected_line,
                        concrete_lines.get(&entry.family),
                    );
                    // A priced winner is rankable by policy. For an unpriced
                    // winner, the lexical tail is deterministic but not
                    // evidence: warn only when every signal-tied pointer disagrees.
                    let alias_choice_is_definite =
                        alias.entry.model.pricing.completion_per_1k.is_some()
                            || alias_candidates
                                .get(&entry.family)
                                .is_none_or(|candidates| {
                                    candidates
                                        .iter()
                                        .filter(|candidate| {
                                            alias_rank_signals_equal(candidate, alias)
                                        })
                                        .all(|candidate| {
                                            alias_line_definitely_disagrees(
                                                &candidate.parsed.product_line,
                                                &selected_line,
                                                concrete_lines.get(&entry.family),
                                            )
                                        })
                                });
                    if alias_disagrees && alias_choice_is_definite {
                        entry.alias_disagreement = Some(alias.entry.model.id.clone());
                    }
                }
                entry
            })
            .collect();
        entries.sort_by(|a, b| a.family.cmp(&b.family));
        entries
    }
}

fn alias_rank_signals_equal(candidate: &RankedFrontierEntry, best: &RankedFrontierEntry) -> bool {
    candidate.entry.tier == best.entry.tier
        && compare_completion_price(&candidate.entry.model, &best.entry.model).is_eq()
        && candidate.entry.model.context_length.unwrap_or(0)
            == best.entry.model.context_length.unwrap_or(0)
}

fn alias_line_definitely_disagrees(
    alias_line: &str,
    selected_line: &str,
    concrete_lines: Option<&HashSet<String>>,
) -> bool {
    !selected_line.is_empty()
        && resolve_alias_product_line(alias_line, concrete_lines)
            .is_some_and(|resolved| resolved != selected_line)
}

fn resolve_alias_product_line<'a>(
    alias_line: &'a str,
    concrete_lines: Option<&HashSet<String>>,
) -> Option<&'a str> {
    if alias_line.is_empty() {
        return None;
    }
    let Some(concrete_lines) = concrete_lines else {
        return Some(alias_line);
    };

    if concrete_lines.contains(alias_line) {
        return Some(alias_line);
    }

    if let Some(line) = concrete_lines
        .iter()
        .filter(|line| {
            alias_line
                .strip_prefix(line.as_str())
                .is_some_and(|suffix| suffix.starts_with('-'))
        })
        .max_by(|a, b| a.len().cmp(&b.len()).then_with(|| b.cmp(a)))
    {
        let resolved_len = line.len();
        return Some(&alias_line[..resolved_len]);
    }

    // A generic pointer stem such as `claude-latest` may cover several
    // concrete lines (`claude-opus`, `claude-sonnet`). Its target line is not
    // knowable from registry metadata, so observability must fail quiet.
    if concrete_lines.iter().any(|line| {
        line.strip_prefix(alias_line)
            .is_some_and(|suffix| suffix.starts_with('-'))
    }) {
        return None;
    }

    Some(alias_line)
}

#[derive(Debug, Clone)]
struct RankedFrontierEntry {
    entry: FrontierEntry,
    parsed: ParsedModelId,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ParsedModelId {
    family: String,
    product_line: String,
    version: Option<ModelVersion>,
    alias: bool,
}

impl ParsedModelId {
    fn parse(id: &str) -> Option<Self> {
        let concrete_id = id.strip_prefix('~').unwrap_or(id);
        let (family, model_name) = concrete_id.split_once('/')?;
        let model_name = model_name.split(':').next().unwrap_or(model_name);
        let alias = id.starts_with('~') || model_name.ends_with("-latest");
        let model_name = model_name.strip_suffix("-latest").unwrap_or(model_name);
        let tokens: Vec<&str> = model_name.split(['-', '_']).collect();
        let numeric_token = tokens
            .iter()
            .position(|token| token.chars().any(|ch| ch.is_ascii_digit()));

        let mut line = Vec::new();
        for token in tokens.iter().take(numeric_token.unwrap_or(tokens.len())) {
            line.push(*token);
        }
        if let Some(token) = numeric_token.and_then(|index| tokens.get(index)) {
            if let Some(numeric_start) = token.find(|ch: char| ch.is_ascii_digit()) {
                let prefix = &token[..numeric_start];
                if !prefix.is_empty() && prefix != "v" {
                    line.push(prefix);
                }
            }
        }

        let product_line = if line.is_empty() {
            model_name.to_string()
        } else {
            line.join("-")
        };

        Some(Self {
            family: family.to_string(),
            product_line,
            version: ModelVersion::parse(&tokens, numeric_token),
            alias,
        })
    }
}

/// Product versions use decimal notation rather than semantic-version
/// segments: 4.5 is newer than 4.20, and 5 equals 5.0.
#[derive(Debug, Clone, Eq, PartialEq)]
struct ModelVersion {
    major: u32,
    fraction: String,
}

impl ModelVersion {
    fn parse(tokens: &[&str], numeric_token: Option<usize>) -> Option<Self> {
        let index = numeric_token?;
        let token = tokens.get(index)?;
        let numeric_start = token.find(|ch: char| ch.is_ascii_digit())?;
        let numeric = &token[numeric_start..];
        if !numeric.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
            return None;
        }

        let mut parts = numeric.split('.');
        let major_text = parts.next()?;
        if major_text.len() >= 4 {
            return None;
        }
        let major = major_text.parse().ok()?;
        let mut fraction = parts.collect::<String>();
        if fraction.is_empty() {
            fraction = tokens
                .get(index + 1)
                .filter(|next| next.len() < 4 && next.chars().all(|ch| ch.is_ascii_digit()))
                .copied()
                .unwrap_or_default()
                .to_string();
        }
        while fraction.ends_with('0') {
            fraction.pop();
        }

        Some(Self { major, fraction })
    }
}

impl Ord for ModelVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| compare_decimal_fractions(&self.fraction, &other.fraction))
    }
}

impl PartialOrd for ModelVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn within_product_line_is_better(
    candidate: &ModelEntry,
    candidate_tier: ModelTier,
    candidate_parsed: &ParsedModelId,
    existing: &ModelEntry,
    existing_tier: ModelTier,
    existing_parsed: &ParsedModelId,
) -> bool {
    candidate_parsed
        .version
        .cmp(&existing_parsed.version)
        .then_with(|| serving_variant_rank(&existing.id).cmp(&serving_variant_rank(&candidate.id)))
        .then(candidate_tier.cmp(&existing_tier))
        .then_with(|| compare_completion_price(candidate, existing))
        .then(candidate.created.cmp(&existing.created))
        .then(
            candidate
                .context_length
                .unwrap_or(0)
                .cmp(&existing.context_length.unwrap_or(0)),
        )
        .then(existing.id.len().cmp(&candidate.id.len()))
        .then_with(|| existing.id.cmp(&candidate.id))
        .is_gt()
}

/// Serving/deployment decorations, not capability tiers. Extend this policy
/// only when the catalog introduces another concrete serving-only suffix.
const SERVING_VARIANT_SUFFIXES: &[&str] = &["customtools", "fast"];

fn serving_variant_rank(id: &str) -> u8 {
    model_name(id)
        .rsplit('-')
        .next()
        .is_some_and(|suffix| SERVING_VARIANT_SUFFIXES.contains(&suffix)) as u8
}

fn model_name(id: &str) -> &str {
    id.rsplit_once('/')
        .map_or(id, |(_, name)| name)
        .split(':')
        .next()
        .unwrap_or(id)
}

fn across_product_lines_is_better(candidate: &FrontierEntry, existing: &FrontierEntry) -> bool {
    candidate
        .tier
        .cmp(&existing.tier)
        .then_with(|| compare_completion_price(&candidate.model, &existing.model))
        .then(
            candidate
                .model
                .context_length
                .unwrap_or(0)
                .cmp(&existing.model.context_length.unwrap_or(0)),
        )
        .then(existing.model.id.len().cmp(&candidate.model.id.len()))
        .then_with(|| existing.model.id.cmp(&candidate.model.id))
        .is_gt()
}

fn compare_completion_price(candidate: &ModelEntry, existing: &ModelEntry) -> Ordering {
    candidate
        .pricing
        .completion_per_1k
        .unwrap_or(0.0)
        .total_cmp(&existing.pricing.completion_per_1k.unwrap_or(0.0))
}

fn compare_decimal_fractions(candidate: &str, existing: &str) -> Ordering {
    let width = candidate.len().max(existing.len());
    let candidate = format!("{candidate:0<width$}");
    let existing = format!("{existing:0<width$}");
    candidate.cmp(&existing)
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
                created: Some(100),
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
                created: None,
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
        assert_eq!(entry.created, Some(100));
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
    fn frontier_excludes_aliases_and_returns_concrete_models() {
        let mut reg = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "openai/gpt-5.6-sol-pro".to_string(),
                    max_output_tokens: Some(128_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.020),
                        ..Default::default()
                    },
                    provider: Some("openrouter".to_string()),
                    ..Default::default()
                },
                ModelEntry {
                    id: "~openai/gpt-latest".to_string(),
                    max_output_tokens: Some(128_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.030),
                        ..Default::default()
                    },
                    provider: Some("openrouter".to_string()),
                    ..Default::default()
                },
                ModelEntry {
                    id: "~openai/internal-pointer".to_string(),
                    max_output_tokens: Some(128_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.004),
                        ..Default::default()
                    },
                    provider: Some("openrouter".to_string()),
                    ..Default::default()
                },
                ModelEntry {
                    id: "openai/gpt-chat-latest".to_string(),
                    max_output_tokens: Some(128_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.030),
                        ..Default::default()
                    },
                    provider: Some("openrouter".to_string()),
                    ..Default::default()
                },
                ModelEntry {
                    id: "google/gemini-3.1-pro-preview".to_string(),
                    max_output_tokens: Some(64_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.012),
                        ..Default::default()
                    },
                    provider: Some("openrouter".to_string()),
                    ..Default::default()
                },
                ModelEntry {
                    id: "~google/gemini-pro-latest".to_string(),
                    max_output_tokens: Some(64_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.012),
                        ..Default::default()
                    },
                    provider: Some("openrouter".to_string()),
                    ..Default::default()
                },
                ModelEntry {
                    id: "x-ai/grok-4.5".to_string(),
                    max_output_tokens: Some(64_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.003),
                        ..Default::default()
                    },
                    provider: Some("openrouter".to_string()),
                    ..Default::default()
                },
                ModelEntry {
                    id: "~x-ai/grok-latest".to_string(),
                    max_output_tokens: Some(64_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.006),
                        ..Default::default()
                    },
                    provider: Some("openrouter".to_string()),
                    ..Default::default()
                },
                ModelEntry {
                    id: "z-ai/glm-5.2".to_string(),
                    max_output_tokens: Some(128_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.002),
                        ..Default::default()
                    },
                    provider: Some("openrouter".to_string()),
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
                .find(|entry| entry.family == "openai")
                .unwrap()
                .model
                .id,
            "openai/gpt-5.6-sol-pro"
        );
        assert_eq!(
            frontier
                .iter()
                .find(|entry| entry.family == "google")
                .unwrap()
                .model
                .id,
            "google/gemini-3.1-pro-preview"
        );
        assert_eq!(
            frontier
                .iter()
                .find(|entry| entry.family == "x-ai")
                .unwrap()
                .model
                .id,
            "x-ai/grok-4.5"
        );
        assert_eq!(
            frontier
                .iter()
                .find(|entry| entry.family == "z-ai")
                .unwrap()
                .model
                .id,
            "z-ai/glm-5.2"
        );
        assert!(
            frontier.iter().all(|entry| {
                !entry.family.starts_with('~') && !entry.model.id.starts_with('~')
            }),
            "frontier entries must be concrete models, never moving aliases"
        );
    }

    fn frontier_alias_observability_registry(aliases: &[(&str, f64)]) -> ModelRegistry {
        let mut models = vec![ModelEntry {
            id: "openai/gpt-5.6-sol-pro".to_string(),
            context_length: Some(128_000),
            max_output_tokens: Some(32_000),
            pricing: ModelPricing {
                completion_per_1k: Some(0.03),
                ..Default::default()
            },
            ..Default::default()
        }];
        models.extend(aliases.iter().map(|(id, price)| ModelEntry {
            id: (*id).to_string(),
            max_output_tokens: Some(32_000),
            pricing: ModelPricing {
                completion_per_1k: Some(*price),
                ..Default::default()
            },
            ..Default::default()
        }));

        ModelRegistry {
            models,
            ..Default::default()
        }
    }

    fn serialized_openai_frontier(aliases: &[(&str, f64)]) -> serde_json::Value {
        let registry = frontier_alias_observability_registry(aliases);
        serde_json::to_value(
            registry
                .frontier()
                .into_iter()
                .find(|entry| entry.family == "openai")
                .expect("openai frontier entry"),
        )
        .expect("frontier entry serializes")
    }

    #[test]
    fn frontier_omits_alias_disagreement_when_best_pointer_agrees() {
        let frontier = serialized_openai_frontier(&[("~openai/gpt-latest", 0.04)]);

        assert!(frontier.get("alias_disagreement").is_none());
    }

    #[test]
    fn frontier_exposes_exact_disagreeing_best_pointer() {
        let frontier = serialized_openai_frontier(&[("openai/chatgpt-latest", 0.04)]);

        assert_eq!(
            frontier.get("alias_disagreement"),
            Some(&serde_json::json!("openai/chatgpt-latest"))
        );
    }

    #[test]
    fn frontier_ignores_lower_ranked_disagreeing_pointer() {
        let frontier = serialized_openai_frontier(&[
            ("~openai/chatgpt-mini-latest", 0.50),
            ("openai/chatgpt-latest", 0.02),
            ("~openai/gpt-latest", 0.04),
        ]);

        assert!(frontier.get("alias_disagreement").is_none());
    }

    #[test]
    fn frontier_relates_pointer_stem_to_known_concrete_product_line() {
        let registry = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "google/gemini-3.1-pro-preview".to_string(),
                    max_output_tokens: Some(32_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.03),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ModelEntry {
                    id: "~google/gemini-pro-latest".to_string(),
                    max_output_tokens: Some(32_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.04),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let frontier = serde_json::to_value(&registry.frontier()[0]).unwrap();
        assert!(frontier.get("alias_disagreement").is_none());
    }

    #[test]
    fn frontier_keeps_distinct_pointer_line_as_definite_disagreement() {
        let registry = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "anthropic/claude-opus-4-6".to_string(),
                    max_output_tokens: Some(32_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.03),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ModelEntry {
                    id: "~anthropic/claude-fable-latest".to_string(),
                    max_output_tokens: Some(32_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.04),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let frontier = serde_json::to_value(&registry.frontier()[0]).unwrap();
        assert_eq!(
            frontier.get("alias_disagreement"),
            Some(&serde_json::json!("~anthropic/claude-fable-latest"))
        );
    }

    #[test]
    fn frontier_fails_quiet_when_unpriced_best_pointer_is_ambiguous() {
        let registry = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "openai/gpt-5.6-sol-pro".to_string(),
                    max_output_tokens: Some(32_000),
                    ..Default::default()
                },
                ModelEntry {
                    id: "~openai/x-latest".to_string(),
                    max_output_tokens: Some(32_000),
                    ..Default::default()
                },
                ModelEntry {
                    id: "~openai/gpt-latest".to_string(),
                    max_output_tokens: Some(32_000),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let frontier = serde_json::to_value(&registry.frontier()[0]).unwrap();
        assert!(frontier.get("alias_disagreement").is_none());
    }

    #[test]
    fn frontier_warns_when_every_unpriced_best_pointer_disagrees() {
        let registry = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "openai/gpt-5.6-sol-pro".to_string(),
                    max_output_tokens: Some(32_000),
                    ..Default::default()
                },
                ModelEntry {
                    id: "~openai/x-latest".to_string(),
                    max_output_tokens: Some(32_000),
                    ..Default::default()
                },
                ModelEntry {
                    id: "~openai/y-latest".to_string(),
                    max_output_tokens: Some(32_000),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let frontier = serde_json::to_value(&registry.frontier()[0]).unwrap();
        assert_eq!(
            frontier.get("alias_disagreement"),
            Some(&serde_json::json!("~openai/x-latest"))
        );
    }

    #[test]
    fn frontier_omits_alias_disagreement_without_pointers() {
        let frontier = serialized_openai_frontier(&[]);

        assert!(frontier.get("alias_disagreement").is_none());
    }

    #[test]
    fn frontier_compares_versions_only_within_product_lines() {
        let mut reg = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "google/gemma-4-31b-it".to_string(),
                    created: Some(200),
                    max_output_tokens: Some(32_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.00035),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ModelEntry {
                    id: "google/gemini-3.1-pro-preview".to_string(),
                    created: Some(100),
                    max_output_tokens: Some(64_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.012),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ModelEntry {
                    id: "x-ai/grok-4.20".to_string(),
                    created: Some(200),
                    max_output_tokens: Some(64_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.003),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ModelEntry {
                    id: "x-ai/grok-4.5".to_string(),
                    created: Some(100),
                    max_output_tokens: Some(64_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.006),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ModelEntry {
                    id: "z-ai/glm-5-turbo".to_string(),
                    created: Some(200),
                    max_output_tokens: Some(128_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.004),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ModelEntry {
                    id: "z-ai/glm-5.2".to_string(),
                    created: Some(100),
                    max_output_tokens: Some(128_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.0011),
                        ..Default::default()
                    },
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
                .find(|entry| entry.family == "google")
                .unwrap()
                .model
                .id,
            "google/gemini-3.1-pro-preview"
        );
        assert_eq!(
            frontier
                .iter()
                .find(|entry| entry.family == "x-ai")
                .unwrap()
                .model
                .id,
            "x-ai/grok-4.5"
        );
        assert_eq!(
            frontier
                .iter()
                .find(|entry| entry.family == "z-ai")
                .unwrap()
                .model
                .id,
            "z-ai/glm-5.2"
        );
    }

    #[test]
    fn frontier_decimal_versions_use_fraction_semantics_without_release_dates() {
        let mut reg = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "x-ai/grok-4.20".to_string(),
                    max_output_tokens: Some(64_000),
                    ..Default::default()
                },
                ModelEntry {
                    id: "x-ai/grok-4.5".to_string(),
                    max_output_tokens: Some(64_000),
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
                .find(|entry| entry.family == "x-ai")
                .unwrap()
                .model
                .id,
            "x-ai/grok-4.5"
        );
    }

    #[test]
    fn model_version_ordering_laws() {
        let version = |id: &str| ParsedModelId::parse(id).unwrap().version;
        let cases = [
            ("test/model-4.5", "test/model-4.20", Ordering::Greater),
            ("test/model-5.2", "test/model-5.1", Ordering::Greater),
            ("test/model-5.1", "test/model-5", Ordering::Greater),
            ("test/model-5", "test/model-5.0", Ordering::Equal),
            ("test/model-3.1", "test/model-4", Ordering::Less),
            ("test/model", "test/model-1", Ordering::Less),
            (
                "anthropic/claude-opus-4-8",
                "anthropic/claude-opus-4.8",
                Ordering::Equal,
            ),
        ];

        for (left, right, expected) in cases {
            assert_eq!(
                version(left).cmp(&version(right)),
                expected,
                "{left} vs {right}"
            );
        }
    }

    #[test]
    fn frontier_uses_created_only_after_equal_versions_within_a_line() {
        let mut reg = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "vendor/model-5.2-snapshot-a".to_string(),
                    created: Some(200),
                    max_output_tokens: Some(8_192),
                    ..Default::default()
                },
                ModelEntry {
                    id: "vendor/model-5.2-snapshot-b".to_string(),
                    created: Some(100),
                    max_output_tokens: Some(8_192),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        reg.rebuild_index();

        let frontier = reg.frontier();
        assert_eq!(frontier[0].model.id, "vendor/model-5.2-snapshot-a");
    }

    #[test]
    fn frontier_prefers_canonical_base_models_over_newer_serving_variants() {
        let mut reg = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "google/gemini-3.1-pro-preview".to_string(),
                    created: Some(100),
                    context_length: Some(1_048_576),
                    max_output_tokens: Some(65_536),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.012),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ModelEntry {
                    id: "google/gemini-3.1-pro-preview-customtools".to_string(),
                    created: Some(200),
                    context_length: Some(1_048_576),
                    max_output_tokens: Some(65_536),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.012),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ModelEntry {
                    id: "anthropic/claude-opus-4.8".to_string(),
                    created: Some(100),
                    context_length: Some(1_000_000),
                    max_output_tokens: Some(128_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.05),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ModelEntry {
                    id: "anthropic/claude-opus-4.8-fast".to_string(),
                    created: Some(200),
                    context_length: Some(1_000_000),
                    max_output_tokens: Some(128_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.05),
                        ..Default::default()
                    },
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
                .find(|entry| entry.family == "google")
                .unwrap()
                .model
                .id,
            "google/gemini-3.1-pro-preview"
        );
        assert_eq!(
            frontier
                .iter()
                .find(|entry| entry.family == "anthropic")
                .unwrap()
                .model
                .id,
            "anthropic/claude-opus-4.8"
        );
    }

    #[test]
    fn frontier_keeps_legitimate_tier_variants_and_skips_latest_pointers() {
        let mut reg = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "openai/gpt-5.6-luna".to_string(),
                    created: Some(100),
                    max_output_tokens: Some(128_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.006),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ModelEntry {
                    id: "openai/gpt-5.6-luna-pro".to_string(),
                    created: Some(200),
                    max_output_tokens: Some(128_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.006),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ModelEntry {
                    id: "openai/gpt-5.6-sol-pro".to_string(),
                    created: Some(50),
                    max_output_tokens: Some(128_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.03),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ModelEntry {
                    id: "openai/gpt-chat-latest".to_string(),
                    created: Some(400),
                    max_output_tokens: Some(128_000),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.03),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        reg.rebuild_index();

        let frontier = reg.frontier();
        assert_eq!(frontier[0].model.id, "openai/gpt-5.6-sol-pro");
    }

    #[test]
    fn within_line_ranking_is_stable_across_input_permutations() {
        let models = [
            ModelEntry {
                id: "vendor/model-5.2".to_string(),
                created: Some(100),
                max_output_tokens: Some(8_192),
                ..Default::default()
            },
            ModelEntry {
                id: "vendor/model-5.2-fast".to_string(),
                created: Some(300),
                max_output_tokens: Some(8_192),
                ..Default::default()
            },
            ModelEntry {
                id: "vendor/model-5.2-snapshot-b".to_string(),
                created: Some(200),
                max_output_tokens: Some(8_192),
                ..Default::default()
            },
        ];
        let permutations = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];

        for order in permutations {
            let mut reg = ModelRegistry {
                models: order
                    .into_iter()
                    .map(|index| models[index].clone())
                    .collect(),
                ..Default::default()
            };
            reg.rebuild_index();
            assert_eq!(reg.frontier()[0].model.id, "vendor/model-5.2-snapshot-b");
        }
    }

    #[test]
    fn frontier_ignores_version_and_created_across_product_lines() {
        let mut reg = ModelRegistry {
            models: vec![
                ModelEntry {
                    id: "vendor/alpha-9".to_string(),
                    created: Some(200),
                    context_length: Some(8_192),
                    max_output_tokens: Some(8_192),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.01),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ModelEntry {
                    id: "vendor/gamma-1".to_string(),
                    created: Some(100),
                    context_length: Some(16_384),
                    max_output_tokens: Some(8_192),
                    pricing: ModelPricing {
                        completion_per_1k: Some(0.01),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        reg.rebuild_index();

        let frontier = reg.frontier();
        assert_eq!(frontier[0].model.id, "vendor/gamma-1");
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
    fn model_created_serialization_roundtrip() {
        let mut registry = ModelRegistry::default();
        registry.models.push(ModelEntry {
            id: "openai/gpt-latest".to_string(),
            created: Some(1_783_590_854),
            ..Default::default()
        });

        let json = serde_json::to_string(&registry).unwrap();
        let roundtrip: ModelRegistry = serde_json::from_str(&json).unwrap();

        assert_eq!(
            roundtrip
                .find("openai/gpt-latest")
                .and_then(|model| model.created),
            Some(1_783_590_854)
        );
    }
}
