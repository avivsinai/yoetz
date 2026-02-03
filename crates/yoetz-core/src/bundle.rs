use crate::types::{Bundle, BundleFile, BundleStats};
use anyhow::{Context, Result};
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
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
    // OverrideBuilder uses whitelist semantics for positive patterns.
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

        let metadata = fs::metadata(path).with_context(|| format!("stat file {rel_path}"))?;
        let file_size = metadata.len() as usize;
        let truncated_by_size = file_size > options.max_file_bytes;

        let data = read_prefix(path, options.max_file_bytes)
            .with_context(|| format!("read file {rel_path}"))?;
        let (mut content, mut truncated, is_binary) =
            extract_text(&data, options.max_file_bytes, truncated_by_size);

        if is_binary && !options.include_binary {
            files.push(BundleFile {
                path: rel_path,
                bytes: file_size,
                sha256: sha256_hex_file(path)?,
                truncated,
                is_binary,
                content: None,
            });
            continue;
        }

        let mut content_len = content.as_ref().map(|c| c.len()).unwrap_or(0);
        if content_len > 0 && total_bytes + content_len > options.max_total_bytes {
            content = Some("[omitted: exceeds max_total_bytes]".to_string());
            truncated = true;
            content_len = content.as_ref().map(|c| c.len()).unwrap_or(0);
        }

        total_chars += content_len;
        total_bytes += content_len;

        files.push(BundleFile {
            path: rel_path,
            bytes: file_size,
            sha256: sha256_hex_file(path)?,
            truncated,
            is_binary,
            content,
        });
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));

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

fn extract_text(
    data: &[u8],
    max_bytes: usize,
    truncated_by_size: bool,
) -> (Option<String>, bool, bool) {
    let slice = if truncated_by_size {
        &data[..max_bytes.min(data.len())]
    } else {
        data
    };

    if slice.contains(&0) {
        return (None, truncated_by_size, true);
    }

    match std::str::from_utf8(slice) {
        Ok(s) => (Some(s.to_string()), truncated_by_size, false),
        Err(e) if truncated_by_size && e.valid_up_to() > 0 => {
            let valid = e.valid_up_to();
            let s = std::str::from_utf8(&slice[..valid]).unwrap_or("");
            (Some(s.to_string()), true, false)
        }
        Err(_) => (None, truncated_by_size, true),
    }
}

fn read_prefix(path: &std::path::Path, max_bytes: usize) -> anyhow::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; max_bytes];
    let read = file.read(&mut buf)?;
    buf.truncate(read);
    Ok(buf)
}

fn sha256_hex_file(path: &std::path::Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    let digest = hasher.finalize();
    Ok(hex::encode(digest))
}

pub fn estimate_tokens(chars: usize) -> usize {
    // Rough heuristic: 4 chars per token.
    chars.div_ceil(4)
}
