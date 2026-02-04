use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::media::MediaOutput;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub prompt: String,
    pub files: Vec<BundleFile>,
    pub stats: BundleStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleFile {
    pub path: String,
    pub bytes: usize,
    pub sha256: String,
    pub truncated: bool,
    pub is_binary: bool,
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BundleStats {
    pub file_count: usize,
    pub total_bytes: usize,
    pub total_chars: usize,
    pub estimated_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PricingEstimate {
    pub estimate_usd: Option<f64>,
    pub input_tokens: Option<usize>,
    pub output_tokens: Option<usize>,
    pub pricing_source: Option<String>,
    pub warnings: Vec<String>,
}

/// Token usage statistics from an LLM call.
///
/// Uses `u64` for token counts to match API response types and avoid
/// truncation issues across different platforms.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub thoughts_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

impl Usage {
    /// Create a new Usage with all fields set to None.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add another Usage to this one, summing all token counts and costs.
    pub fn add(&mut self, other: &Usage) {
        if let Some(input) = other.input_tokens {
            self.input_tokens = Some(self.input_tokens.unwrap_or(0) + input);
        }
        if let Some(output) = other.output_tokens {
            self.output_tokens = Some(self.output_tokens.unwrap_or(0) + output);
        }
        if let Some(thoughts) = other.thoughts_tokens {
            self.thoughts_tokens = Some(self.thoughts_tokens.unwrap_or(0) + thoughts);
        }
        if let Some(total) = other.total_tokens {
            self.total_tokens = Some(self.total_tokens.unwrap_or(0) + total);
        }
        if let Some(cost) = other.cost_usd {
            self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + cost);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub id: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub bundle: Option<Bundle>,
    pub pricing: PricingEstimate,
    pub usage: Usage,
    pub content: String,
    pub artifacts: ArtifactPaths,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ArtifactPaths {
    pub session_dir: String,
    pub bundle_json: Option<String>,
    pub bundle_md: Option<String>,
    pub response_json: Option<String>,
    pub media_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaGenerationResult {
    pub id: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub prompt: String,
    pub usage: Usage,
    pub artifacts: ArtifactPaths,
    pub outputs: Vec<MediaOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleResult {
    pub id: String,
    pub bundle: Bundle,
    pub artifacts: ArtifactPaths,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub path: PathBuf,
}
