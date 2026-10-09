use crate::protocol::ManifestEntry;
use crate::sync::path_safety::is_safe_relative_path;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct ManifestCache {
    pub entries: HashMap<String, CacheEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CacheEntry {
    pub size: u64,
    pub mtime_secs: i64,
    pub mtime_nanos: u32,
    pub blake3_hash: String,
}

impl ManifestCache {
    pub fn load_from_file(path: &Path) -> Self {
        if let Ok(bytes) = fs::read(path) {
            if let Ok(cache) = serde_json::from_slice(&bytes) {
                return cache;
            }
        }

        Self::default()
    }

    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(path, bytes)?;
        Ok(())
    }
}

/// Lists files in the git repository using `git ls-files -co --exclude-standard`.
/// Returns an error if the directory is not a git repository.
pub fn collect_repository_files(repo_root: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("ls-files")
        .arg("-co")
        .arg("-z")
        .arg("--exclude-standard")
        .current_dir(repo_root)
        .output()
        .with_context(|| {
            format!(
                "Failed to execute 'git ls-files' in {}",
                repo_root.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Repository source root '{}' must be a git repository: git ls-files exited with {}: {}",
            repo_root.display(),
            output.status,
            stderr.trim()
        );
    }

    let mut list = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect::<Vec<_>>();
    let staged = Command::new("git")
        .args(["ls-files", "--stage", "-z"])
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("Failed to inspect git index in {}", repo_root.display()))?;
    if !staged.status.success() {
        anyhow::bail!(
            "Failed to inspect git index in {}: {}",
            repo_root.display(),
            String::from_utf8_lossy(&staged.stderr).trim()
        );
    }
    let gitlinks = staged
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|record| {
            let tab = record.iter().position(|byte| *byte == b'\t')?;
            let metadata = std::str::from_utf8(&record[..tab]).ok()?;
            (metadata.starts_with("160000 "))
                .then(|| String::from_utf8_lossy(&record[tab + 1..]).into_owned())
        })
        .collect::<std::collections::HashSet<_>>();
    list.retain(|path| {
        if gitlinks.contains(path) {
            eprintln!("rbuild: Warning: skipping Git submodule '{path}' (gitlink)");
            false
        } else {
            true
        }
    });
    Ok(list)
}

/// Builds the manifest of files in `repo_root`, utilizing `cache` to avoid
/// recomputing BLAKE3 hashes for unmodified files.
pub fn build_manifest(
    repo_root: &Path,
    cache: &mut ManifestCache,
    extra_excludes: &[String],
) -> Result<Vec<ManifestEntry>> {
    let files = collect_repository_files(repo_root)?;
    let mut entries = Vec::new();
    let mut new_cache_entries = HashMap::new();

    for rel_str in files {
        // Enforce path safety
        if is_safe_relative_path(&rel_str).is_err() {
            continue;
        }

        // Check extra exclude patterns
        if extra_excludes
            .iter()
            .any(|pattern| rel_str.starts_with(pattern))
        {
            continue;
        }
        if rel_str.starts_with(".git/") || rel_str == ".git" || rel_str.starts_with(".rbuild") {
            continue;
        }

        let full_path = repo_root.join(&rel_str);
        let meta = match fs::symlink_metadata(&full_path) {
            Ok(m) => m,
            Err(error) => {
                eprintln!(
                    "rbuild: Warning: skipping '{}' (cannot inspect: {error})",
                    full_path.display()
                );
                continue;
            }
        };

        let file_type = meta.file_type();
        let is_symlink = file_type.is_symlink();
        if !is_symlink && !file_type.is_file() {
            eprintln!(
                "rbuild: Warning: skipping '{}' (not a regular file)",
                full_path.display()
            );
            continue;
        }
        let mode = meta.permissions().mode();

        let (size, mtime_secs, mtime_nanos, hash, symlink_target) = if is_symlink {
            let target_path = match fs::read_link(&full_path) {
                Ok(target) => target,
                Err(error) => {
                    eprintln!(
                        "rbuild: Warning: skipping '{}' (cannot read symlink: {error})",
                        full_path.display()
                    );
                    continue;
                }
            };
            let target_str = target_path.to_string_lossy().to_string();
            let hash = blake3::hash(target_str.as_bytes()).to_hex().to_string();
            (0u64, 0i64, 0u32, hash, Some(target_str))
        } else {
            let size = meta.len();
            let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let duration = mtime
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default();
            let mtime_secs = duration.as_secs() as i64;
            let mtime_nanos = duration.subsec_nanos();

            // Cache check
            let hash = if let Some(cached) = cache.entries.get(&rel_str) {
                if cached.size == size
                    && cached.mtime_secs == mtime_secs
                    && cached.mtime_nanos == mtime_nanos
                {
                    cached.blake3_hash.clone()
                } else {
                    let content = match fs::read(&full_path) {
                        Ok(content) => content,
                        Err(error) => {
                            eprintln!(
                                "rbuild: Warning: skipping '{}' (cannot read: {error})",
                                full_path.display()
                            );
                            continue;
                        }
                    };
                    blake3::hash(&content).to_hex().to_string()
                }
            } else {
                let content = match fs::read(&full_path) {
                    Ok(content) => content,
                    Err(error) => {
                        eprintln!(
                            "rbuild: Warning: skipping '{}' (cannot read: {error})",
                            full_path.display()
                        );
                        continue;
                    }
                };
                blake3::hash(&content).to_hex().to_string()
            };

            new_cache_entries.insert(
                rel_str.clone(),
                CacheEntry {
                    size,
                    mtime_secs,
                    mtime_nanos,
                    blake3_hash: hash.clone(),
                },
            );

            (size, mtime_secs, mtime_nanos, hash, None)
        };

        entries.push(ManifestEntry {
            path: rel_str,
            mode,
            size,
            mtime_secs,
            mtime_nanos,
            is_symlink,
            symlink_target,
            blake3_hash: hash,
        });
    }

    cache.entries = new_cache_entries;
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_git_repo(path: &Path) {
        Command::new("git")
            .arg("init")
            .current_dir(path)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(path)
            .output()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(path)
            .output()
            .expect("git config user");
    }

    #[test]
    fn test_manifest_creation_and_caching() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.txt");
        fs::write(&file_a, b"content a").unwrap();
        fs::write(&file_b, b"content b").unwrap();

        let mut cache = ManifestCache::default();
        let entries = build_manifest(dir.path(), &mut cache, &[]).unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.path == "a.txt"));
        assert!(entries.iter().any(|e| e.path == "b.txt"));

        // Second run with cache
        let entries2 = build_manifest(dir.path(), &mut cache, &[]).unwrap();
        assert_eq!(entries2.len(), 2);
        assert_eq!(entries, entries2);
    }

    #[test]
    fn test_manifest_symlink() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        fs::write(&target, b"target content").unwrap();
        std::os::unix::fs::symlink("target.txt", &link).unwrap();

        let mut cache = ManifestCache::default();
        let entries = build_manifest(dir.path(), &mut cache, &[]).unwrap();

        let sym_entry = entries.iter().find(|e| e.path == "link.txt").unwrap();
        assert!(sym_entry.is_symlink);
        assert_eq!(sym_entry.symlink_target.as_deref(), Some("target.txt"));
    }

    #[test]
    fn gitlink_is_skipped_and_directory_entries_do_not_abort_manifest() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        fs::write(dir.path().join("kept.txt"), b"keep").unwrap();
        fs::create_dir_all(dir.path().join("module")).unwrap();
        fs::write(dir.path().join("module/inside.txt"), b"submodule content").unwrap();
        let submodule = tempfile::tempdir().unwrap();
        init_git_repo(submodule.path());
        fs::write(submodule.path().join("sub.txt"), b"submodule").unwrap();
        for args in [vec!["add", "sub.txt"], vec!["commit", "-m", "submodule"]] {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(submodule.path())
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let oid = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(submodule.path())
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let add = Command::new("git")
            .args([
                "update-index",
                "--add",
                "--cacheinfo",
                "160000",
                &oid,
                "module",
            ])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(add.success());

        let mut cache = ManifestCache::default();
        let entries = build_manifest(dir.path(), &mut cache, &[]).unwrap();
        assert!(entries.iter().any(|entry| entry.path == "kept.txt"));
        assert!(
            !entries
                .iter()
                .any(|entry| entry.path == "module" || entry.path.starts_with("module/"))
        );
    }

    #[test]
    fn test_manifest_fails_on_non_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        let file_a = dir.path().join("a.txt");
        fs::write(&file_a, b"content").unwrap();

        let mut cache = ManifestCache::default();
        let res = build_manifest(dir.path(), &mut cache, &[]);
        assert!(res.is_err(), "Non-git directory must return an error");
    }
}
