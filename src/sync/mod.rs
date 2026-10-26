pub mod apply;
pub mod diff;
pub mod manifest;
pub mod path_safety;

pub use apply::{apply_deletions, apply_file_chunk};
pub use diff::{
    DiffResult, WORKTREE_MANIFEST_CACHE_REL_PATH, calculate_diff, calculate_diff_with_cache,
    record_synced_file_in_cache, worktree_cache_path,
};
pub use manifest::{ManifestCache, build_manifest, collect_repository_files};
pub use path_safety::{
    PathSafetyError, is_safe_relative_path, resolve_safe_path, verify_symlink_target,
};
