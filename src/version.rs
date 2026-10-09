use std::path::Path;

pub const PROTOCOL_VERSION: u32 = 1;

/// Returns the build ID of the current binary or environment.
///
/// If running as an installed executable, it computes the BLAKE3 hash of
/// `std::env::current_exe()`. If reading the executable fails (e.g. in tests
/// or memory-only environments), it falls back to a deterministic string
/// based on the crate version and compile-time information.
pub fn get_current_build_id() -> String {
    if let Ok(exe_path) = std::env::current_exe() {
        if exe_path.exists() {
            if let Ok(hash) = compute_file_blake3(&exe_path) {
                return hash;
            }
        }
    }

    format!("{}-dev-fallback", env!("CARGO_PKG_VERSION"))
}

pub fn compute_file_blake3(path: &Path) -> Result<String, std::io::Error> {
    let bytes = std::fs::read(path)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_id_not_empty() {
        let id = get_current_build_id();
        assert!(!id.is_empty(), "Build ID must not be empty");
    }

    #[test]
    fn test_compute_file_blake3() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, b"hello rbuild").unwrap();

        let hash = compute_file_blake3(&file_path).unwrap();
        let expected = blake3::hash(b"hello rbuild").to_hex().to_string();
        assert_eq!(hash, expected);
    }
}
