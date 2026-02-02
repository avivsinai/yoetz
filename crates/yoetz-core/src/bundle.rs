use crate::types::{Bundle, BundleFile, BundleStats};
use anyhow::{Context, Result};
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct BundleOptions {
    pub root: PathBuf,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
    pub include_hidden: bool,
    pub include_binary: bool,
}

impl Default for BundleOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            include: Vec::new(),
            exclude: Vec::new(),
            max_file_bytes: 200_000,
            max_total_bytes: 5_000_000,
            include_hidden: false,
            include_binary: false,
        }
    }
}

pub fn build_bundle(prompt: &str, options: BundleOptions) -> Result<Bundle> {
    let mut override_builder = OverrideBuilder::new(&options.root);
    for pattern in &options.include {
        override_builder.add(pattern)?;
    }
    for pattern in &options.exclude {
        override_builder.add(&format!("!{}", pattern))?;
    }
    let overrides = override_builder.build()?;

    let mut walker = WalkBuilder::new(&options.root);
    walker
        .hidden(!options.include_hidden)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .ignore(true)
        .overrides(overrides);

    let mut files = Vec::new();
    let mut total_bytes = 0usize;
    let mut total_chars = 0usize;

    for entry in walker.build() {
        let entry = entry?;
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }

        let path = entry.path();
        let rel_path = path
            .strip_prefix(&options.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let data = fs::read(path).with_context(|| format!("read file {rel_path}"))?;
        if total_bytes + data.len() > options.max_total_bytes {
            break;
        }

        let (content, truncated, is_binary) = extract_text(&data, options.max_file_bytes);
        if is_binary && !options.include_binary {
            files.push(BundleFile {
                path: rel_path,
                bytes: data.len(),
                sha256: sha256_hex(&data),
                truncated: false,
                is_binary,
                content: None,
            });
            total_bytes += data.len();
            continue;
        }

        let content_len = content.as_ref().map(|c| c.len()).unwrap_or(0);
        total_chars += content_len;
        total_bytes += data.len();

        files.push(BundleFile {
            path: rel_path,
            bytes: data.len(),
            sha256: sha256_hex(&data),
            truncated,
            is_binary,
            content,
        });
    }

    let stats = BundleStats {
        file_count: files.len(),
        total_bytes,
        total_chars,
        estimated_tokens: estimate_tokens(prompt.len() + total_chars),
    };

    Ok(Bundle {
        prompt: prompt.to_string(),
        files,
        stats,
    })
}

fn extract_text(data: &[u8], max_bytes: usize) -> (Option<String>, bool, bool) {
    let truncated = data.len() > max_bytes;
    let slice = if truncated { &data[..max_bytes] } else { data };

    if slice.contains(&0) {
        return (None, truncated, true);
    }

    match std::str::from_utf8(slice) {
        Ok(s) => (Some(s.to_string()), truncated, false),
        Err(_) => (None, truncated, true),
    }
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    hex::encode(digest)
}

pub fn estimate_tokens(chars: usize) -> usize {
    // Rough heuristic: 4 chars per token.
    chars.div_ceil(4)
}
