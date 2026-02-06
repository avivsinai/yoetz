use crate::types::{Bundle, BundleFile, BundleStats};
use anyhow::{Context, Result};
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

/// Options for building a file bundle for LLM context.
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

/// Walk the filesystem and collect files into a [`Bundle`] for LLM context.
///
/// Respects `.gitignore`, include/exclude globs, and size limits.
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

        let (data, sha256, file_size) = read_prefix_and_hash(path, options.max_file_bytes)
            .with_context(|| format!("read file {rel_path}"))?;
        let truncated_by_size = file_size > options.max_file_bytes;
        let (mut content, mut truncated, is_binary) =
            extract_text(&data, options.max_file_bytes, truncated_by_size);

        if is_binary && !options.include_binary {
            files.push(BundleFile {
                path: rel_path,
                bytes: file_size,
                sha256,
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
            sha256,
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

fn read_prefix_and_hash(
    path: &std::path::Path,
    max_bytes: usize,
) -> anyhow::Result<(Vec<u8>, String, usize)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    let mut prefix = Vec::with_capacity(std::cmp::min(max_bytes, buf.len()));
    let mut total = 0usize;
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        total += read;
        hasher.update(&buf[..read]);
        if prefix.len() < max_bytes {
            let remaining = max_bytes - prefix.len();
            let take = remaining.min(read);
            prefix.extend_from_slice(&buf[..take]);
        }
    }
    let digest = hasher.finalize();
    Ok((prefix, hex::encode(digest), total))
}

/// Rough token count estimate (~4 chars per token).
pub fn estimate_tokens(chars: usize) -> usize {
    // Rough heuristic: 4 chars per token.
    chars.div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::extract_text;
    use super::{build_bundle, BundleOptions};
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn extract_text_truncates_utf8_safely() {
        let text = "hello 🙂 world";
        let bytes = text.as_bytes();
        let cut = bytes.iter().position(|b| *b == 0xF0).unwrap_or(bytes.len());
        let data = &bytes[..cut + 1];
        let (content, truncated, is_binary) = extract_text(data, data.len(), true);
        assert!(truncated);
        assert!(!is_binary);
        let content = content.expect("expected utf-8 content");
        assert!(content.starts_with("hello "));
    }

    #[test]
    fn bundle_files_sorted_and_hash_full_file() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("yoetz_bundle_test_{nanos}"));
        fs::create_dir_all(&root).unwrap();

        let a_path = root.join("a.txt");
        let b_path = root.join("b.txt");
        fs::write(&b_path, "bbb").unwrap();
        fs::write(&a_path, "aaa").unwrap();

        let options = BundleOptions {
            root: root.clone(),
            include: vec!["**/*".to_string()],
            max_file_bytes: 2, // force truncation
            ..BundleOptions::default()
        };

        let bundle = build_bundle("prompt", options).unwrap();
        let paths: Vec<_> = bundle.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "b.txt"]);

        let mut hasher = Sha256::new();
        hasher.update(b"aaa");
        let a_hash = hex::encode(hasher.finalize());
        let file_a = bundle.files.iter().find(|f| f.path == "a.txt").unwrap();
        assert_eq!(file_a.sha256, a_hash);

        let _ = fs::remove_dir_all(&root);
    }
}
