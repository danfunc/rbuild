use crate::protocol::ManifestEntry;
use crate::sync::path_safety::{is_safe_relative_path, resolve_safe_path};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct DiffResult {
    pub needed_files: Vec<String>,
    pub deleted_files: Vec<String>,
}

/// Computes the diff between the incoming client manifest and the agent's workdir.
///
/// Returns:
/// - `needed_files`: Files that must be sent by client (missing, content modified, mode changed, or symlink changed).
/// - `deleted_files`: Files present in agent workdir but absent in client manifest.
pub fn calculate_diff(workdir: &Path, client_entries: &[ManifestEntry]) -> Result<DiffResult> {
    let mut needed = Vec::new();
    let mut client_paths = HashSet::new();
    let mut client_map: HashMap<&str, &ManifestEntry> = HashMap::new();

    for entry in client_entries {
        client_paths.insert(entry.path.as_str());
        client_map.insert(entry.path.as_str(), entry);

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
            if meta.len() != entry.size {
                needed.push(entry.path.clone());
                continue;
            }

            // Compute blake3
            match fs::read(&safe_target) {
                Ok(bytes) => {
                    let hash = blake3::hash(&bytes).to_hex().to_string();
                    if hash != entry.blake3_hash {
                        needed.push(entry.path.clone());
                    }
                }
                Err(_) => needed.push(entry.path.clone()),
            }
        }
    }

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

fn collect_existing_files(root: &Path, current: &Path, acc: &mut Vec<String>) -> Result<()> {
    if !current.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip internal tracking files
        if name.starts_with(".rbuild") || name == ".git" {
            continue;
        }

        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_existing_files(root, &path, acc)?;
        } else {
            if let Ok(rel) = path.strip_prefix(root) {
                acc.push(rel.to_string_lossy().to_string());
            }
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
}
