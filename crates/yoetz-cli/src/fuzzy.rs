use serde::Serialize;
use yoetz_core::registry::ModelRegistry;

#[derive(Debug, Clone, Serialize)]
pub struct FuzzyMatch {
    pub id: String,
    pub score: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_length: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<usize>,
}

/// Search the registry for models matching `query`, returning up to `max_results`
/// sorted by descending score.
pub fn fuzzy_search(registry: &ModelRegistry, query: &str, max_results: usize) -> Vec<FuzzyMatch> {
    let mut matches: Vec<FuzzyMatch> = registry
        .models
        .iter()
        .filter_map(|entry| {
            let score = score_model(query, &entry.id);
            if score >= 50 {
                Some(FuzzyMatch {
                    id: entry.id.clone(),
                    score,
                    provider: entry.provider.clone(),
                    context_length: entry.context_length,
                    max_output_tokens: entry.max_output_tokens,
                })
            } else {
                None
            }
        })
        .collect();
    // Sort by score desc, then shorter ID first for ties
    matches.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.id.len().cmp(&b.id.len()))
    });
    matches.truncate(max_results);
    matches
}

/// Return the best match above the resolution threshold (score >= 200).
#[allow(dead_code)]
pub fn fuzzy_resolve(registry: &ModelRegistry, query: &str) -> Option<FuzzyMatch> {
    let results = fuzzy_search(registry, query, 1);
    results.into_iter().next().filter(|m| m.score >= 200)
}

/// Score how well `query` matches `candidate`. Higher is better. Returns 0 for no match.
pub(crate) fn score_model(query: &str, candidate: &str) -> u32 {
    let q = query.to_lowercase();
    let c = candidate.to_lowercase();

    // Exact match
    if q == c {
        return 1000;
    }

    // Split into prefix/name on first `/`
    let (q_prefix, q_name) = split_provider(&q);
    let (c_prefix, c_name) = split_provider(&c);

    let mut score: u32 = 0;

    // Exact name match (ignoring prefix), or full unprefixed query matches candidate name
    if q_name == c_name || (q_prefix.is_none() && q.as_str() == c_name) {
        score = score.max(800);
    }
    // Name starts-with
    else if c_name.starts_with(q_name) || (q_prefix.is_none() && c_name.starts_with(&q)) {
        score = score.max(600);
    }
    // Substring contains
    else if c_name.contains(q_name) || (q_prefix.is_none() && c_name.contains(q.as_str())) {
        score = score.max(400);
    }

    // Token overlap scoring
    let q_tokens = tokenize(q_name);
    let c_tokens = tokenize(c_name);
    if !q_tokens.is_empty() && !c_tokens.is_empty() {
        let overlap = q_tokens.iter().filter(|t| c_tokens.contains(t)).count();
        if overlap > 0 {
            // Scale: 200 base + 150 * (overlap ratio)
            let ratio = overlap as f64 / q_tokens.len().max(c_tokens.len()) as f64;
            let token_score = 200 + (150.0 * ratio) as u32;
            score = score.max(token_score);
        }
    }

    // Edit distance on name part
    let edit_dist = levenshtein(q_name, c_name);
    let name_len = q_name.len().max(c_name.len());
    if name_len > 0 && edit_dist <= name_len / 3 + 1 {
        // Scale: 300 for distance 0 (handled by exact above), down to 100
        let dist_score = if edit_dist == 0 {
            300
        } else {
            let frac = edit_dist as f64 / name_len as f64;
            (300.0 * (1.0 - frac)) as u32
        };
        let dist_score = dist_score.max(100);
        score = score.max(dist_score);
    }

    // Provider prefix bonus
    if let (Some(qp), Some(cp)) = (q_prefix, c_prefix) {
        if qp == cp {
            score = score.saturating_add(50);
        }
    }

    // Floor: discard scores below 50
    if score < 50 {
        0
    } else {
        score
    }
}

fn split_provider(s: &str) -> (Option<&str>, &str) {
    match s.split_once('/') {
        Some((prefix, rest)) => (Some(prefix), rest),
        None => (None, s),
    }
}

fn tokenize(s: &str) -> Vec<&str> {
    s.split(['-', '/', '.']).filter(|t| !t.is_empty()).collect()
}

/// Compute Levenshtein edit distance between two strings.
/// Uses O(min(a,b)) space with a single-row DP approach.
pub(crate) fn levenshtein(a: &str, b: &str) -> usize {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let a_len = a_bytes.len();
    let b_len = b_bytes.len();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    // Ensure b is the shorter string for O(min) space
    if a_len < b_len {
        return levenshtein(b, a);
    }

    let mut prev_row: Vec<usize> = (0..=b_len).collect();
    let mut curr_row = vec![0usize; b_len + 1];

    for i in 1..=a_len {
        curr_row[0] = i;
        for j in 1..=b_len {
            let cost = if a_bytes[i - 1] == b_bytes[j - 1] {
                0
            } else {
                1
            };
            curr_row[j] = (prev_row[j] + 1)
                .min(curr_row[j - 1] + 1)
                .min(prev_row[j - 1] + cost);
        }
        std::mem::swap(&mut prev_row, &mut curr_row);
    }

    prev_row[b_len]
}

#[cfg(test)]
mod tests {
    use super::*;
    use yoetz_core::registry::{ModelEntry, ModelRegistry};

    fn test_registry() -> ModelRegistry {
        let mut reg = ModelRegistry::default();
        reg.models = vec![
            ModelEntry {
                id: "x-ai/grok-4".to_string(),
                provider: Some("openrouter".to_string()),
                context_length: Some(131072),
                max_output_tokens: Some(16384),
                ..Default::default()
            },
            ModelEntry {
                id: "x-ai/grok-4-mini".to_string(),
                provider: Some("openrouter".to_string()),
                context_length: Some(131072),
                max_output_tokens: Some(16384),
                ..Default::default()
            },
            ModelEntry {
                id: "anthropic/claude-sonnet-4".to_string(),
                provider: Some("openrouter".to_string()),
                ..Default::default()
            },
            ModelEntry {
                id: "anthropic/claude-opus-4".to_string(),
                provider: Some("openrouter".to_string()),
                ..Default::default()
            },
            ModelEntry {
                id: "openai/gpt-5.2".to_string(),
                provider: Some("openrouter".to_string()),
                ..Default::default()
            },
            ModelEntry {
                id: "google/gemini-3-pro-preview".to_string(),
                provider: Some("openrouter".to_string()),
                ..Default::default()
            },
            ModelEntry {
                id: "google/gemini-3-flash-preview".to_string(),
                provider: Some("openrouter".to_string()),
                ..Default::default()
            },
        ];
        reg.rebuild_index();
        reg
    }

    #[test]
    fn exact_match() {
        let reg = test_registry();
        let results = fuzzy_search(&reg, "x-ai/grok-4", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "x-ai/grok-4");
        assert_eq!(results[0].score, 1000);
    }

    #[test]
    fn partial_name_grok() {
        let reg = test_registry();
        let results = fuzzy_search(&reg, "grok-4", 5);
        assert!(!results.is_empty());
        // Should match x-ai/grok-4 with high score (name match)
        assert_eq!(results[0].id, "x-ai/grok-4");
        assert!(results[0].score >= 600);
    }

    #[test]
    fn wrong_version() {
        // grok-4.1 doesn't exist, should suggest grok-4
        let reg = test_registry();
        let resolved = fuzzy_resolve(&reg, "grok-4.1");
        assert!(resolved.is_some());
        let m = resolved.unwrap();
        assert_eq!(m.id, "x-ai/grok-4");
    }

    #[test]
    fn missing_prefix() {
        let reg = test_registry();
        let results = fuzzy_search(&reg, "claude-sonnet-4", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "anthropic/claude-sonnet-4");
    }

    #[test]
    fn case_insensitive() {
        let reg = test_registry();
        let results = fuzzy_search(&reg, "X-AI/GROK-4", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "x-ai/grok-4");
        assert_eq!(results[0].score, 1000);
    }

    #[test]
    fn multiple_results() {
        let reg = test_registry();
        let results = fuzzy_search(&reg, "claude", 10);
        assert!(results.len() >= 2);
        // Both claude models should be present
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"anthropic/claude-sonnet-4"));
        assert!(ids.contains(&"anthropic/claude-opus-4"));
    }

    #[test]
    fn no_match() {
        let reg = test_registry();
        let results = fuzzy_search(&reg, "nonexistent-model-xyz", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn levenshtein_correctness() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", "xyz"), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("saturday", "sunday"), 3);
        assert_eq!(levenshtein("grok-4.1", "grok-4"), 2);
        assert_eq!(levenshtein("grok-4", "grok-4"), 0);
    }

    #[test]
    fn provider_prefixed_query() {
        let reg = test_registry();
        let results = fuzzy_search(&reg, "openai/gpt-5.2", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "openai/gpt-5.2");
        assert_eq!(results[0].score, 1000);
    }

    #[test]
    fn fuzzy_resolve_threshold() {
        let reg = test_registry();
        // "grok" alone should still resolve since token overlap gives >= 200
        let resolved = fuzzy_resolve(&reg, "grok");
        assert!(resolved.is_some());
        let m = resolved.unwrap();
        assert!(m.id.contains("grok"));
    }

    #[test]
    fn fuzzy_resolve_returns_none_for_garbage() {
        let reg = test_registry();
        let resolved = fuzzy_resolve(&reg, "zzzznotamodel");
        assert!(resolved.is_none());
    }

    #[test]
    fn claude_sonnet_partial() {
        let reg = test_registry();
        let results = fuzzy_search(&reg, "claude-sonnet", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "anthropic/claude-sonnet-4");
    }

    #[test]
    fn gemini_search() {
        let reg = test_registry();
        let results = fuzzy_search(&reg, "gemini-3-pro", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "google/gemini-3-pro-preview");
    }
}
