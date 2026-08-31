use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

fn main() {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by Cargo"),
    );
    let extension_dir = manifest_dir
        .join("..")
        .join("..")
        .join("extensions/chatgpt-native");
    let files = package_file_paths(&extension_dir).unwrap_or_else(|error| {
        panic!(
            "could not fingerprint ChatGPT native extension at {}: {error}",
            extension_dir.display()
        )
    });

    for relative in &files {
        println!(
            "cargo:rerun-if-changed={}",
            extension_dir.join(relative).display()
        );
    }

    let fingerprint = package_fingerprint(&extension_dir, &files).unwrap_or_else(|error| {
        panic!(
            "could not fingerprint ChatGPT native extension at {}: {error}",
            extension_dir.display()
        )
    });
    println!("cargo:rustc-env=YOETZ_CHATGPT_NATIVE_EXTENSION_FINGERPRINT={fingerprint}");
}

fn package_file_paths(root: &Path) -> Result<Vec<PathBuf>, String> {
    let root_files = [
        "manifest.json",
        "native-host-manifest.template.json",
        "popup.html",
        "popup.js",
    ];
    let mut files = Vec::new();
    for relative in root_files {
        let path = PathBuf::from(relative);
        if !root.join(&path).is_file() {
            return Err(format!("missing package file {}", path.display()));
        }
        files.push(path);
    }
    for directory in ["icons", "src"] {
        collect_files(root, Path::new(directory), &mut files)?;
    }
    files.sort();
    Ok(files)
}

fn collect_files(root: &Path, relative_dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let directory = root.join(relative_dir);
    let entries = fs::read_dir(&directory)
        .map_err(|error| format!("read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("read entry in {}: {error}", directory.display()))?;
        let relative = relative_dir.join(entry.file_name());
        let path = root.join(&relative);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("package contains symlink {}", path.display()));
        }
        if metadata.is_dir() {
            collect_files(root, &relative, files)?;
        } else if metadata.is_file() {
            files.push(relative);
        } else {
            return Err(format!(
                "package contains unsupported file {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn package_fingerprint(root: &Path, files: &[PathBuf]) -> Result<String, String> {
    let mut hash = Sha256::new();
    for relative in files {
        let path = root.join(relative);
        let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
        hash.update(normalized_relative_path(relative).as_bytes());
        hash.update([0]);
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
        hash.update([0]);
    }
    Ok(hex::encode(hash.finalize()))
}

fn normalized_relative_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
