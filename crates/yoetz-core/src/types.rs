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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub input_tokens: Option<usize>,
    pub output_tokens: Option<usize>,
    pub total_tokens: Option<usize>,
    pub cost_usd: Option<f64>,
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
