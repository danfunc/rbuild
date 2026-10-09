use crate::protocol::message::SyncFileChunk;
use crate::sync::path_safety::{resolve_safe_path, verify_symlink_target};
use anyhow::{Context, Result, bail};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Applies a single incoming file chunk to the agent's workdir.
///
/// Ensures:
/// 1. Path safety and base directory containment.
/// 2. BLAKE3 verification and raw length verification.
/// 3. Atomic writing via temporary file and rename.
/// 4. Permission modes and symlink targets are preserved safely.
pub fn apply_file_chunk(workdir: &Path, chunk: &SyncFileChunk) -> Result<()> {
    let target_path = resolve_safe_path(workdir, &chunk.path)
        .with_context(|| format!("Validating path: {}", chunk.path))?;

    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Creating parent directory: {}", parent.display()))?;
    }

    if chunk.is_symlink {
        let symlink_target = chunk
            .symlink_target
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Missing symlink target for {}", chunk.path))?;

        verify_symlink_target(workdir, &target_path, symlink_target)
            .with_context(|| format!("Symlink escape validation failed for {}", chunk.path))?;

        // Remove existing file/symlink if present
        if target_path.exists() || fs::symlink_metadata(&target_path).is_ok() {
            let _ = fs::remove_file(&target_path);
        }

        std::os::unix::fs::symlink(symlink_target, &target_path)
            .with_context(|| format!("Creating symlink: {}", target_path.display()))?;

        return Ok(());
    }

    // Normal file: Decompress using zstd
    let decompressed = zstd::decode_all(&chunk.zstd_data[..])
        .with_context(|| format!("Decompressing zstd data for {}", chunk.path))?;

    if decompressed.len() as u64 != chunk.raw_len {
        bail!(
            "Length mismatch for {}: expected {}, got {}",
            chunk.path,
            chunk.raw_len,
            decompressed.len()
        );
    }

    let actual_hash = blake3::hash(&decompressed).to_hex().to_string();
    if actual_hash != chunk.blake3 {
        bail!(
            "BLAKE3 checksum mismatch for {}: expected {}, got {}",
            chunk.path,
            chunk.blake3,
            actual_hash
        );
    }

    // Atomic write using a sibling temporary file
    let parent = target_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Missing parent directory"))?;
    let tmp_file = tempfile::Builder::new()
        .prefix(".rbuild_sync_tmp_")
        .tempfile_in(parent)
        .with_context(|| format!("Creating tempfile in {}", parent.display()))?;

    fs::write(tmp_file.path(), &decompressed)
        .with_context(|| format!("Writing data to tempfile for {}", chunk.path))?;

    // Set permission mode (respecting executable bit)
    let permissions = fs::Permissions::from_mode(chunk.mode);
    fs::set_permissions(tmp_file.path(), permissions)
        .with_context(|| format!("Setting permissions for {}", chunk.path))?;

    // Atomic persist/rename
    tmp_file
        .persist(&target_path)
        .with_context(|| format!("Persisting file to {}", target_path.display()))?;

    Ok(())
}

/// Safely removes deleted files from the workdir.
pub fn apply_deletions(workdir: &Path, deleted_files: &[String]) -> Result<usize> {
    let mut count = 0;
    for rel_path in deleted_files {
        let safe_target = match resolve_safe_path(workdir, rel_path) {
            Ok(p) => p,
            Err(_) => continue,
        };

        if (safe_target.exists() || fs::symlink_metadata(&safe_target).is_ok())
            && fs::remove_file(&safe_target).is_ok()
        {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_file_chunk_atomic_and_hash_verified() {
        let dir = tempfile::tempdir().unwrap();
        let content = b"hello synced world";
        let raw_len = content.len() as u64;
        let hash = blake3::hash(content).to_hex().to_string();
        let zstd_data = zstd::encode_all(&content[..], 3).unwrap();

        let chunk = SyncFileChunk {
            path: "nested/dir/test.txt".to_string(),
            mode: 0o755,
            is_symlink: false,
            symlink_target: None,
            raw_len,
            blake3: hash.clone(),
            zstd_data,
        };

        apply_file_chunk(dir.path(), &chunk).unwrap();

        let target = dir.path().join("nested/dir/test.txt");
        assert!(target.exists());
        let read_back = fs::read(&target).unwrap();
        assert_eq!(read_back, content);

        let meta = fs::metadata(&target).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o755);
    }

    #[test]
    fn test_apply_file_chunk_checksum_mismatch_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let content = b"legit content";
        let zstd_data = zstd::encode_all(&content[..], 3).unwrap();

        let chunk = SyncFileChunk {
            path: "corrupt.txt".to_string(),
            mode: 0o644,
            is_symlink: false,
            symlink_target: None,
            raw_len: content.len() as u64,
            blake3: "corrupted_hash_value".to_string(),
            zstd_data,
        };

        let result = apply_file_chunk(dir.path(), &chunk);
        assert!(result.is_err(), "Must reject on BLAKE3 mismatch");
    }

    #[test]
    fn test_apply_symlink_safe() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("source.txt"), b"src").unwrap();

        let chunk = SyncFileChunk {
            path: "link.txt".to_string(),
            mode: 0o777,
            is_symlink: true,
            symlink_target: Some("source.txt".to_string()),
            raw_len: 0,
            blake3: blake3::hash(b"source.txt").to_hex().to_string(),
            zstd_data: Vec::new(),
        };

        apply_file_chunk(dir.path(), &chunk).unwrap();

        let link_path = dir.path().join("link.txt");
        assert!(
            fs::symlink_metadata(&link_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&link_path).unwrap(), Path::new("source.txt"));
    }

    #[test]
    fn test_apply_deletions() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("to_delete.txt");
        fs::write(&file, b"bye").unwrap();

        let deleted = vec!["to_delete.txt".to_string()];
        let count = apply_deletions(dir.path(), &deleted).unwrap();

        assert_eq!(count, 1);
        assert!(!file.exists());
    }
}
