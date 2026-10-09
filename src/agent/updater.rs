use anyhow::{Context, Result, bail};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Atomically replaces the target agent binary after verifying checksum and size.
pub fn update_agent_binary(
    target_bin_path: &Path,
    compressed_data: &[u8],
    raw_len: u64,
    expected_blake3: &str,
) -> Result<()> {
    let decompressed =
        zstd::decode_all(compressed_data).context("Decompressing new agent binary with zstd")?;

    if decompressed.len() as u64 != raw_len {
        bail!(
            "Binary length mismatch: expected {}, got {}",
            raw_len,
            decompressed.len()
        );
    }

    let actual_blake3 = blake3::hash(&decompressed).to_hex().to_string();
    if actual_blake3 != expected_blake3 {
        bail!(
            "Agent binary BLAKE3 hash mismatch: expected {}, got {}",
            expected_blake3,
            actual_blake3
        );
    }

    let parent_dir = target_bin_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Missing parent directory for agent binary"))?;
    fs::create_dir_all(parent_dir)
        .with_context(|| format!("Creating agent bin directory: {}", parent_dir.display()))?;

    // Create temporary file in the same directory for atomic rename
    let tmp_file = tempfile::Builder::new()
        .prefix(".rbuild_agent_update_")
        .tempfile_in(parent_dir)
        .context("Creating temp file for agent update")?;

    fs::write(tmp_file.path(), &decompressed).context("Writing binary data to temp file")?;

    // Grant executable permissions (0o755)
    let permissions = fs::Permissions::from_mode(0o755);
    fs::set_permissions(tmp_file.path(), permissions)
        .context("Setting executable permissions (0o755)")?;

    // Atomically persist/rename over target binary
    tmp_file
        .persist(target_bin_path)
        .with_context(|| format!("Persisting updated binary to {}", target_bin_path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_agent_binary_success() {
        let dir = tempfile::tempdir().unwrap();
        let bin_path = dir.path().join("rbuild_agent");

        let binary_bytes = b"fake binary executable content";
        let raw_len = binary_bytes.len() as u64;
        let hash = blake3::hash(binary_bytes).to_hex().to_string();
        let zstd_data = zstd::encode_all(&binary_bytes[..], 3).unwrap();

        update_agent_binary(&bin_path, &zstd_data, raw_len, &hash).unwrap();

        assert!(bin_path.exists());
        let read = fs::read(&bin_path).unwrap();
        assert_eq!(read, binary_bytes);

        let meta = fs::metadata(&bin_path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o755);
    }

    #[test]
    fn test_update_agent_binary_checksum_failure() {
        let dir = tempfile::tempdir().unwrap();
        let bin_path = dir.path().join("rbuild_agent");

        let binary_bytes = b"fake binary";
        let zstd_data = zstd::encode_all(&binary_bytes[..], 3).unwrap();

        let res = update_agent_binary(
            &bin_path,
            &zstd_data,
            binary_bytes.len() as u64,
            "wrong_hash_string",
        );
        assert!(res.is_err(), "Must reject update if hash does not match");
        assert!(!bin_path.exists());
    }
}
