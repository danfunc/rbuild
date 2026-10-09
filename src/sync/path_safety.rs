use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum PathSafetyError {
    #[error("Path is empty or consists of only whitespace")]
    EmptyPath,

    #[error("Absolute paths are forbidden: '{0}'")]
    AbsolutePathForbidden(String),

    #[error("Path traversal detected (e.g. '..'): '{0}'")]
    PathTraversalDetected(String),

    #[error("Invalid character (e.g. null byte or drive prefix) in path: '{0}'")]
    InvalidCharacter(String),

    #[error("Resolved path '{0}' is outside base directory '{1}'")]
    EscapesBaseDir(String, String),

    #[error("Symlink target '{0}' attempts to escape base directory")]
    SymlinkEscapesBaseDir(String),
}

/// Validates whether a relative path is clean, normalized, and cannot traverse
/// outside the target root directory.
pub fn is_safe_relative_path(rel_path: &str) -> Result<(), PathSafetyError> {
    let trimmed = rel_path.trim();
    if trimmed.is_empty() {
        return Err(PathSafetyError::EmptyPath);
    }

    if rel_path.contains('\0') {
        return Err(PathSafetyError::InvalidCharacter(rel_path.to_string()));
    }

    let path = Path::new(rel_path);

    if path.is_absolute() || rel_path.starts_with('/') || rel_path.starts_with('\\') {
        return Err(PathSafetyError::AbsolutePathForbidden(rel_path.to_string()));
    }

    for component in path.components() {
        match component {
            Component::Prefix(_) => {
                return Err(PathSafetyError::InvalidCharacter(rel_path.to_string()));
            }
            Component::RootDir => {
                return Err(PathSafetyError::AbsolutePathForbidden(rel_path.to_string()));
            }
            Component::ParentDir => {
                return Err(PathSafetyError::PathTraversalDetected(rel_path.to_string()));
            }
            Component::CurDir => {
                // Single '.' components within paths are suspicious if standalone
                if rel_path == "." {
                    return Err(PathSafetyError::EmptyPath);
                }
            }
            Component::Normal(_) => {}
        }
    }

    Ok(())
}

/// Resolves a relative path safely under a canonical base directory.
///
/// Ensures the resolved path is strictly contained within `base_dir`.
pub fn resolve_safe_path(base_dir: &Path, rel_path: &str) -> Result<PathBuf, PathSafetyError> {
    is_safe_relative_path(rel_path)?;

    let target = base_dir.join(rel_path);

    // Normalize path components logically without requiring target to exist on disk yet
    let mut normalized = PathBuf::new();
    for comp in target.components() {
        match comp {
            Component::ParentDir => {
                return Err(PathSafetyError::PathTraversalDetected(rel_path.to_string()));
            }
            c => normalized.push(c.as_os_str()),
        }
    }

    // Ensure normalized path starts with base_dir components
    if !normalized.starts_with(base_dir) {
        return Err(PathSafetyError::EscapesBaseDir(
            normalized.display().to_string(),
            base_dir.display().to_string(),
        ));
    }

    Ok(normalized)
}

/// Verifies that a symlink target does not escape the base directory.
pub fn verify_symlink_target(
    base_dir: &Path,
    symlink_file_path: &Path,
    target_str: &str,
) -> Result<(), PathSafetyError> {
    if target_str.contains('\0') || target_str.trim().is_empty() {
        return Err(PathSafetyError::InvalidCharacter(target_str.to_string()));
    }

    let target_path = Path::new(target_str);

    // Absolute symlinks: must start with base_dir
    if target_path.is_absolute() {
        if !target_path.starts_with(base_dir) {
            return Err(PathSafetyError::SymlinkEscapesBaseDir(
                target_str.to_string(),
            ));
        }
        return Ok(());
    }

    // Relative symlinks: calculate resolved destination relative to symlink's parent directory
    let parent_dir = symlink_file_path.parent().unwrap_or(base_dir);
    let resolved = parent_dir.join(target_path);

    // Collapse components
    let mut clean_dest = PathBuf::new();
    for comp in resolved.components() {
        match comp {
            Component::ParentDir => {
                if !clean_dest.pop() {
                    return Err(PathSafetyError::SymlinkEscapesBaseDir(
                        target_str.to_string(),
                    ));
                }
            }
            Component::CurDir => {}
            c => clean_dest.push(c.as_os_str()),
        }
    }

    if !clean_dest.starts_with(base_dir) {
        return Err(PathSafetyError::SymlinkEscapesBaseDir(
            target_str.to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_relative_paths() {
        assert!(is_safe_relative_path("foo.txt").is_ok());
        assert!(is_safe_relative_path("src/lib.rs").is_ok());
        assert!(is_safe_relative_path("nested/dir/file.ext").is_ok());
    }

    #[test]
    fn test_rejects_traversal() {
        assert!(is_safe_relative_path("../secret").is_err());
        assert!(is_safe_relative_path("foo/../../secret").is_err());
        assert!(is_safe_relative_path("foo/../bar").is_err());
    }

    #[test]
    fn test_rejects_absolute_paths() {
        assert!(is_safe_relative_path("/etc/passwd").is_err());
        assert!(is_safe_relative_path("\\windows\\system32").is_err());
    }

    #[test]
    fn test_rejects_empty_and_null() {
        assert!(is_safe_relative_path("").is_err());
        assert!(is_safe_relative_path("   ").is_err());
        assert!(is_safe_relative_path("foo\0bar").is_err());
    }

    #[test]
    fn test_resolve_safe_path() {
        let base = Path::new("/srv/build/client/repo/wt");
        let safe = resolve_safe_path(base, "src/main.rs").unwrap();
        assert_eq!(safe, Path::new("/srv/build/client/repo/wt/src/main.rs"));

        assert!(resolve_safe_path(base, "../escaped.txt").is_err());
    }

    #[test]
    fn test_verify_symlink_target() {
        let base = Path::new("/srv/build/client/repo/wt");
        let symlink_file = base.join("subdir/link.txt");

        // Valid relative link inside tree
        assert!(verify_symlink_target(base, &symlink_file, "target.txt").is_ok());
        assert!(verify_symlink_target(base, &symlink_file, "../other.txt").is_ok());

        // Escaping link
        assert!(verify_symlink_target(base, &symlink_file, "../../escaped.txt").is_err());
        assert!(verify_symlink_target(base, &symlink_file, "/etc/passwd").is_err());
    }
}
