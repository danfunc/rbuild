use serde::{Deserialize, Serialize};

/// Represents a single file or symlink entry in the sync manifest.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ManifestEntry {
    /// Normalized relative path (e.g. "src/main.rs"). Path traversal is strictly forbidden.
    pub path: String,
    /// Unix permission mode (e.g. 0o755, 0o644).
    pub mode: u32,
    /// File size in bytes (0 for symlinks).
    pub size: u64,
    /// Modification timestamp in seconds.
    pub mtime_secs: i64,
    /// Modification timestamp nanoseconds component.
    pub mtime_nanos: u32,
    /// True if this entry represents a symbolic link.
    pub is_symlink: bool,
    /// Symlink target if `is_symlink` is true.
    pub symlink_target: Option<String>,
    /// BLAKE3 hex hash of the content (or symlink target).
    pub blake3_hash: String,
}

/// Represents the content payload for transferring a single file or symlink.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SyncFileChunk {
    pub path: String,
    pub mode: u32,
    pub is_symlink: bool,
    pub symlink_target: Option<String>,
    pub raw_len: u64,
    pub blake3: String,
    pub zstd_data: Vec<u8>,
}

/// Protocol messages exchanged between `rbuild` client and `rbuild agent`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type")]
pub enum Message {
    // ---------------- Handshake ----------------
    HandshakeRequest {
        protocol_version: u32,
        build_id: String,
        #[serde(default)]
        platform: Option<String>,
    },
    HandshakeResponse {
        protocol_version: u32,
        build_id: String,
        compatible: bool,
        #[serde(default)]
        platform: Option<String>,
        message: Option<String>,
    },

    // ---------------- Agent Binary Update ----------------
    UpdateAgentRequest {
        build_id: String,
        raw_len: u64,
        blake3: String,
        zstd_data: Vec<u8>,
    },
    UpdateAgentResponse {
        ok: bool,
        error: Option<String>,
    },

    // ---------------- Worktree Lock & Setup ----------------
    LockWorktreeRequest {
        client_id: String,
        repo: String,
        worktree: String,
    },
    LockWorktreeResponse {
        ok: bool,
        assigned_path: String,
        error: Option<String>,
    },

    // ---------------- Sync Manifest ----------------
    SyncManifestRequest {
        entries: Vec<ManifestEntry>,
    },
    SyncManifestResponse {
        needed_files: Vec<String>,
        deleted_files: Vec<String>,
    },

    // ---------------- Sync File Content ----------------
    SyncFileChunk(SyncFileChunk),
    SyncFileResponse {
        path: String,
        ok: bool,
        error: Option<String>,
    },

    // ---------------- Sync Completion ----------------
    SyncCompleteRequest,
    SyncCompleteResponse {
        ok: bool,
        files_updated: usize,
        files_deleted: usize,
        error: Option<String>,
    },

    // ---------------- Remote Command Execution ----------------
    RunCommandRequest {
        command: String,
        args: Vec<String>,
        envs: Vec<(String, String)>,
    },
    CommandStdout {
        data: Vec<u8>,
    },
    CommandStderr {
        data: Vec<u8>,
    },
    CommandExit {
        exit_code: i32,
    },

    // ---------------- Artifact Collection ----------------
    CollectArtifactsRequest {
        command: Option<String>,
        targets: Vec<String>,
        flags: Vec<String>,
        #[serde(default)]
        envs: Vec<(String, String)>,
        #[serde(default)]
        known_artifacts: std::collections::BTreeMap<String, String>,
    },
    CollectArtifactsResponse {
        ok: bool,
        artifact_count: usize,
        error: Option<String>,
    },
    ArtifactChunk(SyncFileChunk),
    ArtifactUnchanged {
        path: String,
    },
    ArtifactStart {
        path: String,
        mode: u32,
        raw_len: u64,
        blake3: String,
    },
    ArtifactData {
        path: String,
        offset: u64,
        raw_len: u64,
        zstd_data: Vec<u8>,
    },
    ArtifactEnd {
        path: String,
    },
    CollectPathsRequest {
        paths: Vec<String>,
        #[serde(default)]
        known_artifacts: std::collections::BTreeMap<String, String>,
    },
    CollectPathsResponse {
        ok: bool,
        artifact_count: usize,
        error: Option<String>,
    },

    // ---------------- General Error / Ping ----------------
    ErrorResponse {
        code: String,
        message: String,
    },
    Ping,
    Pong,
}

/// Reserved `envs` key. The agent prepends its value (a `:`-separated directory list) to the
/// inherited `PATH` of each remote command; it is never exported under its own name.
pub const PATH_PREPEND_ENV: &str = "RBUILD_PATH_PREPEND";

/// Builds the `envs` entry that carries extra remote PATH directories, or nothing when empty.
pub fn path_prepend_envs(entries: &[String]) -> Vec<(String, String)> {
    let joined: Vec<&str> = entries
        .iter()
        .map(|e| e.trim())
        .filter(|e| !e.is_empty())
        .collect();
    if joined.is_empty() {
        return Vec::new();
    }
    vec![(PATH_PREPEND_ENV.to_string(), joined.join(":"))]
}
