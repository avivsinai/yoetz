use crate::types::{Bundle, BundleFile, BundleStats};
use anyhow::{Context, Result};
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

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

/// Expand `~` or `~/…` to the user's home directory.
fn expand_tilde(path: &str) -> String {
    if path == "~" {
        std::env::var("HOME").unwrap_or_else(|_| path.to_string())
    } else if let Some(rest) = path.strip_prefix("~/") {
        match std::env::var("HOME") {
            Ok(home) => format!("{home}/{rest}"),
            Err(_) => path.to_string(),
        }
    } else {
        path.to_string()
    }
}

/// Returns `true` if the string contains glob metacharacters.
fn has_glob_chars(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[') || s.contains('{')
}

/// Process a single file into a [`BundleFile`] entry.
///
/// Returns `(BundleFile, content_bytes_consumed, content_chars)`.
fn process_file(
    path: &Path,
    display_path: String,
    max_file_bytes: usize,
    max_total_bytes: usize,
    current_total: usize,
    include_binary: bool,
) -> Result<(BundleFile, usize, usize)> {
    let (data, sha256, file_size) = read_prefix_and_hash(path, max_file_bytes)
        .with_context(|| format!("read file {display_path}"))?;
    let truncated_by_size = file_size > max_file_bytes;
    let (mut content, mut truncated, is_binary) =
        extract_text(&data, max_file_bytes, truncated_by_size);

    if is_binary && !include_binary {
        return Ok((
            BundleFile {
                path: display_path,
                bytes: file_size,
                sha256,
                truncated,
                is_binary,
                content: None,
            },
            0,
            0,
        ));
    }

    let mut content_len = content.as_ref().map(|c| c.len()).unwrap_or(0);
    if content_len > 0 && current_total + content_len > max_total_bytes {
        content = Some("[omitted: exceeds max_total_bytes]".to_string());
        truncated = true;
        content_len = content.as_ref().map(|c| c.len()).unwrap_or(0);
    }

    let content_chars = content.as_ref().map(|c| c.chars().count()).unwrap_or(0);

    Ok((
        BundleFile {
            path: display_path,
            bytes: file_size,
            sha256,
            truncated,
            is_binary,
            content,
        },
        content_len,
        content_chars,
    ))
}

/// Walk the filesystem and collect files into a [`Bundle`] for LLM context.
///
/// Respects `.gitignore`, include/exclude globs, and size limits.
/// Handles tilde (`~`) expansion and absolute file paths in include patterns.
pub fn build_bundle(prompt: &str, options: BundleOptions) -> Result<Bundle> {
    // Partition include patterns: absolute literal paths are read directly;
    // everything else (relative paths, globs) goes through the directory walker.
    let mut direct_files: Vec<PathBuf> = Vec::new();
    let mut glob_patterns: Vec<String> = Vec::new();

    for pattern in &options.include {
        let expanded = expand_tilde(pattern);
        if Path::new(&expanded).is_absolute() && !has_glob_chars(&expanded) {
            direct_files.push(PathBuf::from(expanded));
        } else {
            glob_patterns.push(expanded);
        }
    }

    let exclude_expanded: Vec<String> = options.exclude.iter().map(|p| expand_tilde(p)).collect();

    let mut files = Vec::new();
    let mut seen_files = HashSet::new();
    let mut total_bytes = 0usize;
    let mut total_chars = 0usize;

    // 1. Read directly-specified absolute files.
    for file_path in &direct_files {
        if !file_path.is_file() {
            return Err(anyhow::anyhow!(
                "-f path not found or not a file: {}",
                file_path.display()
            ));
        }
        let identity = file_identity(file_path)
            .with_context(|| format!("resolve file {}", file_path.display()))?;
        if !seen_files.insert(identity) {
            continue;
        }
        let display_path = file_path.to_string_lossy().to_string();
        let (bf, consumed_bytes, consumed_chars) = process_file(
            file_path,
            display_path,
            options.max_file_bytes,
            options.max_total_bytes,
            total_bytes,
            options.include_binary,
        )?;
        total_bytes += consumed_bytes;
        total_chars += consumed_chars;
        files.push(bf);
    }

    // 2. Walk the directory tree for glob / relative patterns.
    //    Also walk when include was empty (the "walk everything" case, e.g. --all).
    if !glob_patterns.is_empty() || options.include.is_empty() {
        let mut override_builder = OverrideBuilder::new(&options.root);
        for pattern in &glob_patterns {
            override_builder.add(pattern)?;
        }
        for pattern in &exclude_expanded {
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

        for entry in walker.build() {
            let entry = entry?;
            if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                continue;
            }

            let path = entry.path();
            let identity =
                file_identity(path).with_context(|| format!("resolve file {}", path.display()))?;
            if !seen_files.insert(identity) {
                continue;
            }
            let rel_path = path
                .strip_prefix(&options.root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();

            let (bf, consumed_bytes, consumed_chars) = process_file(
                path,
                rel_path,
                options.max_file_bytes,
                options.max_total_bytes,
                total_bytes,
                options.include_binary,
            )?;
            total_bytes += consumed_bytes;
            total_chars += consumed_chars;
            files.push(bf);
        }
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));

    let stats = BundleStats {
        file_count: files.len(),
        total_bytes,
        total_chars,
        estimated_tokens: estimate_tokens(prompt.chars().count() + total_chars),
    };

    Ok(Bundle {
        prompt: prompt.to_string(),
        files,
        stats,
    })
}

fn file_identity(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("canonicalize {}", path.display()))
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
    use super::{build_bundle, estimate_tokens, expand_tilde, has_glob_chars, BundleOptions};
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

    #[test]
    fn expand_tilde_expands_home() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~/foo/bar"), format!("{home}/foo/bar"));
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("foo/bar"), "foo/bar");
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
    }

    #[test]
    fn test_has_glob_chars() {
        assert!(has_glob_chars("*.rs"));
        assert!(has_glob_chars("src/**/*.rs"));
        assert!(has_glob_chars("file?.txt"));
        assert!(has_glob_chars("file[0-9].txt"));
        assert!(has_glob_chars("{a,b}.txt"));
        assert!(!has_glob_chars("src/main.rs"));
        assert!(!has_glob_chars("/absolute/path/file.rs"));
        assert!(!has_glob_chars("~/file.rs"));
    }

    #[test]
    fn bundle_includes_absolute_path_file() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("yoetz_abs_root_{nanos}"));
        let outside = std::env::temp_dir().join(format!("yoetz_abs_outside_{nanos}"));
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();

        let ext_file = outside.join("external.txt");
        fs::write(&ext_file, "external content").unwrap();

        let options = BundleOptions {
            root: root.clone(),
            include: vec![ext_file.to_string_lossy().to_string()],
            ..BundleOptions::default()
        };

        let bundle = build_bundle("prompt", options).unwrap();
        assert_eq!(bundle.files.len(), 1);
        assert_eq!(bundle.files[0].path, ext_file.to_string_lossy().to_string());
        assert_eq!(bundle.files[0].content.as_deref(), Some("external content"));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn bundle_mixes_absolute_and_glob_patterns() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("yoetz_mix_root_{nanos}"));
        let outside = std::env::temp_dir().join(format!("yoetz_mix_outside_{nanos}"));
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();

        fs::write(root.join("local.txt"), "local").unwrap();
        let ext_file = outside.join("external.txt");
        fs::write(&ext_file, "external").unwrap();

        let options = BundleOptions {
            root: root.clone(),
            include: vec![ext_file.to_string_lossy().to_string(), "*.txt".to_string()],
            ..BundleOptions::default()
        };

        let bundle = build_bundle("prompt", options).unwrap();
        assert_eq!(bundle.files.len(), 2);
        let paths: Vec<_> = bundle.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"local.txt"));
        assert!(paths.contains(&ext_file.to_string_lossy().as_ref()));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn bundle_errors_on_missing_absolute_path() {
        let options = BundleOptions {
            include: vec!["/nonexistent/path/to/file.txt".to_string()],
            ..BundleOptions::default()
        };

        let result = build_bundle("prompt", options);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("/nonexistent/path/to/file.txt"));
    }

    #[test]
    fn bundle_walks_everything_with_empty_include() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("yoetz_all_test_{nanos}"));
        fs::create_dir_all(&root).unwrap();

        fs::write(root.join("a.txt"), "aaa").unwrap();
        fs::write(root.join("b.txt"), "bbb").unwrap();

        let options = BundleOptions {
            root: root.clone(),
            include: vec![], // --all mode: no include patterns
            ..BundleOptions::default()
        };

        let bundle = build_bundle("prompt", options).unwrap();
        assert_eq!(bundle.files.len(), 2);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bundle_dedups_same_file_across_direct_and_glob_inputs() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("yoetz_dedup_test_{nanos}"));
        fs::create_dir_all(&root).unwrap();

        let local = root.join("local.txt");
        fs::write(&local, "local").unwrap();

        let options = BundleOptions {
            root: root.clone(),
            include: vec![local.to_string_lossy().to_string(), "*.txt".to_string()],
            ..BundleOptions::default()
        };

        let bundle = build_bundle("prompt", options).unwrap();
        assert_eq!(bundle.files.len(), 1);
        assert_eq!(bundle.stats.file_count, 1);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bundle_stats_count_unicode_chars_not_bytes() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("yoetz_chars_test_{nanos}"));
        fs::create_dir_all(&root).unwrap();

        let file = root.join("unicode.txt");
        fs::write(&file, "a🙂b").unwrap();

        let bundle = build_bundle(
            "🙂",
            BundleOptions {
                root: root.clone(),
                include: vec!["unicode.txt".to_string()],
                ..BundleOptions::default()
            },
        )
        .unwrap();

        assert_eq!(bundle.stats.total_bytes, "a🙂b".len());
        assert_eq!(bundle.stats.total_chars, "a🙂b".chars().count());
        assert_eq!(
            bundle.stats.estimated_tokens,
            estimate_tokens("🙂".chars().count() + "a🙂b".chars().count())
        );

        let _ = fs::remove_dir_all(&root);
    }
}
