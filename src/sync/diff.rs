use crate::protocol::ManifestEntry;
use crate::sync::manifest::{CacheEntry, ManifestCache};
use crate::sync::path_safety::{is_safe_relative_path, resolve_safe_path};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Relative path inside a remote worktree where the agent stores its manifest cache.
pub const WORKTREE_MANIFEST_CACHE_REL_PATH: &str = ".rbuild/manifest_cache.json";

/// Returns the manifest cache file path inside `workdir`.
pub fn worktree_cache_path(workdir: &Path) -> PathBuf {
    workdir.join(WORKTREE_MANIFEST_CACHE_REL_PATH)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct DiffResult {
    pub needed_files: Vec<String>,
    pub deleted_files: Vec<String>,
}

fn extract_mtime(meta: &fs::Metadata) -> Option<(i64, u32)> {
    let mtime = meta.modified().ok()?;
    let duration = mtime
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok()?;
    Some((duration.as_secs() as i64, duration.subsec_nanos()))
}

/// Computes the diff between the incoming client manifest and the agent's workdir,
/// loading and persisting the worktree manifest cache at `.rbuild/manifest_cache.json`.
///
/// Returns:
/// - `needed_files`: Files that must be sent by client (missing, content modified, mode changed, or symlink changed).
/// - `deleted_files`: Files present in agent workdir but absent in client manifest.
pub fn calculate_diff(workdir: &Path, client_entries: &[ManifestEntry]) -> Result<DiffResult> {
    let cache_path = worktree_cache_path(workdir);
    let mut cache = ManifestCache::load_from_file(&cache_path);
    let diff = calculate_diff_with_cache(workdir, client_entries, &mut cache)?;
    if workdir.exists() {
        let _ = cache.save_to_file(&cache_path);
    }
    Ok(diff)
}

/// Computes the diff between the incoming client manifest and the agent's workdir,
/// using `cache` to avoid reading and hashing files whose size and mtime have not changed.
pub fn calculate_diff_with_cache(
    workdir: &Path,
    client_entries: &[ManifestEntry],
    cache: &mut ManifestCache,
) -> Result<DiffResult> {
    let mut needed = Vec::new();
    let mut client_paths = HashSet::new();
    let mut next_cache_entries = HashMap::new();

    for entry in client_entries {
        client_paths.insert(entry.path.as_str());

        let safe_target = match resolve_safe_path(workdir, &entry.path) {
            Ok(p) => p,
            Err(_) => {
                // If path is invalid or attempts traversal, reject it
                continue;
            }
        };

        if !safe_target.exists() && fs::symlink_metadata(&safe_target).is_err() {
            needed.push(entry.path.clone());
            continue;
        }

        let meta = match fs::symlink_metadata(&safe_target) {
            Ok(m) => m,
            Err(_) => {
                needed.push(entry.path.clone());
                continue;
            }
        };

        let is_sym = meta.file_type().is_symlink();
        if is_sym != entry.is_symlink {
            needed.push(entry.path.clone());
            continue;
        }

        // Compare permission mode (specifically executable bit 0o111)
        let local_mode = meta.permissions().mode();
        let mode_exec_match = (local_mode & 0o111) == (entry.mode & 0o111);
        if !mode_exec_match {
            needed.push(entry.path.clone());
            continue;
        }

        if is_sym {
            match fs::read_link(&safe_target) {
                Ok(link_target) => {
                    let link_str = link_target.to_string_lossy();
                    if entry.symlink_target.as_deref() != Some(link_str.as_ref()) {
                        needed.push(entry.path.clone());
                    }
                }
                Err(_) => needed.push(entry.path.clone()),
            }
        } else {
            // Check size first for quick disqualification
            let size = meta.len();
            if size != entry.size {
                needed.push(entry.path.clone());
                continue;
            }

            let mtime = extract_mtime(&meta);

            // Reuse cached BLAKE3 hash when size and mtime match
            let hash = match (mtime, cache.entries.get(&entry.path)) {
                (Some((mtime_secs, mtime_nanos)), Some(cached))
                    if cached.size == size
                        && cached.mtime_secs == mtime_secs
                        && cached.mtime_nanos == mtime_nanos =>
                {
                    cached.blake3_hash.clone()
                }
                _ => match fs::read(&safe_target) {
                    Ok(bytes) => blake3::hash(&bytes).to_hex().to_string(),
                    Err(_) => {
                        needed.push(entry.path.clone());
                        continue;
                    }
                },
            };

            if let Some((mtime_secs, mtime_nanos)) = mtime {
                next_cache_entries.insert(
                    entry.path.clone(),
                    CacheEntry {
                        size,
                        mtime_secs,
                        mtime_nanos,
                        blake3_hash: hash.clone(),
                    },
                );
            }

            if hash != entry.blake3_hash {
                needed.push(entry.path.clone());
            }
        }
    }

    cache.entries = next_cache_entries;

    // Scan existing files in workdir to find deleted ones
    let mut agent_existing_files = Vec::new();
    collect_existing_files(workdir, workdir, &mut agent_existing_files)?;

    let mut deleted = Vec::new();
    for local_rel in agent_existing_files {
        if is_safe_relative_path(&local_rel).is_ok() && !client_paths.contains(local_rel.as_str()) {
            deleted.push(local_rel);
        }
    }

    Ok(DiffResult {
        needed_files: needed,
        deleted_files: deleted,
    })
}

/// Updates the worktree manifest cache entry for a newly applied file so the next sync
/// does not need to re-read and hash it from disk.
pub fn record_synced_file_in_cache(
    workdir: &Path,
    cache: &mut ManifestCache,
    rel_path: &str,
    blake3_hash: &str,
) {
    let Ok(safe_target) = resolve_safe_path(workdir, rel_path) else {
        return;
    };
    let Ok(meta) = fs::symlink_metadata(&safe_target) else {
        return;
    };
    if !meta.file_type().is_file() {
        cache.entries.remove(rel_path);
        return;
    }
    if let Some((mtime_secs, mtime_nanos)) = extract_mtime(&meta) {
        cache.entries.insert(
            rel_path.to_string(),
            CacheEntry {
                size: meta.len(),
                mtime_secs,
                mtime_nanos,
                blake3_hash: blake3_hash.to_string(),
            },
        );
    }
}

fn collect_existing_files(root: &Path, current: &Path, acc: &mut Vec<String>) -> Result<()> {
    if !current.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip internal tracking files and remote build output directories/symlinks
        if name.starts_with(".rbuild")
            || name == ".git"
            || (current == root && (name == "target" || name.starts_with("bazel-")))
        {
            continue;
        }

        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_existing_files(root, &path, acc)?;
        } else if let Ok(rel) = path.strip_prefix(root) {
            acc.push(rel.to_string_lossy().to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_needed_and_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let keep_file = dir.path().join("keep.txt");
        let delete_file = dir.path().join("delete.txt");

        fs::write(&keep_file, b"keep content").unwrap();
        fs::write(&delete_file, b"to delete").unwrap();

        let keep_hash = blake3::hash(b"keep content").to_hex().to_string();

        let client_entries = vec![
            ManifestEntry {
                path: "keep.txt".to_string(),
                mode: 0o644,
                size: 12,
                mtime_secs: 100,
                mtime_nanos: 0,
                is_symlink: false,
                symlink_target: None,
                blake3_hash: keep_hash,
            },
            ManifestEntry {
                path: "new_file.txt".to_string(),
                mode: 0o644,
                size: 7,
                mtime_secs: 100,
                mtime_nanos: 0,
                is_symlink: false,
                symlink_target: None,
                blake3_hash: "abcd".to_string(),
            },
        ];

        let diff = calculate_diff(dir.path(), &client_entries).unwrap();
        assert_eq!(diff.needed_files, vec!["new_file.txt"]);
        assert_eq!(diff.deleted_files, vec!["delete.txt"]);
    }

    #[test]
    fn test_diff_reuses_remote_mtime_cache_and_persists_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cached.txt");
        fs::write(&file, b"original content").unwrap();
        let real_hash = blake3::hash(b"original content").to_hex().to_string();

        let entries = vec![ManifestEntry {
            path: "cached.txt".to_string(),
            mode: 0o644,
            size: 16,
            mtime_secs: 1,
            mtime_nanos: 0,
            is_symlink: false,
            symlink_target: None,
            blake3_hash: real_hash.clone(),
        }];

        // First run populates .rbuild/manifest_cache.json on the worktree
        let diff1 = calculate_diff(dir.path(), &entries).unwrap();
        assert!(diff1.needed_files.is_empty());
        let cache_file = worktree_cache_path(dir.path());
        assert!(cache_file.exists(), "Remote manifest cache must be saved");

        // Poison the cached hash while keeping the exact on-disk size and mtime:
        // If calculate_diff uses the cache, it will see the poisoned hash and report cached.txt as needed.
        let mut cache = ManifestCache::load_from_file(&cache_file);
        cache.entries.get_mut("cached.txt").unwrap().blake3_hash = "poisoned_hash".to_string();
        cache.save_to_file(&cache_file).unwrap();

        let diff2 = calculate_diff(dir.path(), &entries).unwrap();
        assert_eq!(
            diff2.needed_files,
            vec!["cached.txt"],
            "Matching size and mtime must use cached hash without re-reading disk"
        );
    }

    #[test]
    fn test_diff_preserves_remote_build_outputs() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("target/release")).unwrap();
        fs::write(dir.path().join("target/release/app"), b"binary").unwrap();
        fs::create_dir_all(dir.path().join("bazel-bin/pkg")).unwrap();
        fs::write(dir.path().join("bazel-bin/pkg/out"), b"bazel-out").unwrap();

        let diff = calculate_diff(dir.path(), &[]).unwrap();
        assert!(
            diff.deleted_files.is_empty(),
            "Remote target/ and bazel-* build outputs must not be marked for deletion: {:?}",
            diff.deleted_files
        );
    }
}
