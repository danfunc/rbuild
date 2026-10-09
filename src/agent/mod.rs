pub mod lock;
pub mod updater;

use crate::agent::lock::WorktreeLock;
use crate::agent::updater::update_agent_binary;
use crate::protocol::message::{Message, SyncFileChunk};
use crate::protocol::{read_message, write_message};
use crate::sync::apply::{apply_deletions, apply_file_chunk};
use crate::sync::diff::calculate_diff;
use crate::sync::path_safety::is_safe_relative_path;
use crate::version::{PROTOCOL_VERSION, get_current_build_id};
use anyhow::{Context, Result};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

pub struct AgentSession {
    pub base_dir: PathBuf,
    pub agent_bin_path: Option<PathBuf>,
    pub current_worktree: Option<PathBuf>,
    pub active_lock: Option<WorktreeLock>,
    pub pending_deletions: Vec<String>,
    pub updated_count: usize,
    pub deleted_count: usize,
}

impl AgentSession {
    pub fn new(base_dir: PathBuf, agent_bin_path: Option<PathBuf>) -> Self {
        Self {
            base_dir,
            agent_bin_path,
            current_worktree: None,
            active_lock: None,
            pending_deletions: Vec::new(),
            updated_count: 0,
            deleted_count: 0,
        }
    }

    pub fn handle_message<W: Write>(&mut self, writer: &mut W, msg: Message) -> Result<bool> {
        match msg {
            Message::HandshakeRequest {
                protocol_version,
                build_id,
            } => {
                let local_build_id = get_current_build_id();
                let compatible = protocol_version == PROTOCOL_VERSION && build_id == local_build_id;
                let response = Message::HandshakeResponse {
                    protocol_version: PROTOCOL_VERSION,
                    build_id: local_build_id,
                    compatible,
                    message: if compatible {
                        None
                    } else {
                        Some("Version or build ID mismatch".to_string())
                    },
                };
                write_message(writer, &response)?;
            }

            Message::UpdateAgentRequest {
                build_id: _,
                raw_len,
                blake3,
                zstd_data,
            } => {
                let bin_path = self
                    .agent_bin_path
                    .clone()
                    .or_else(|| std::env::current_exe().ok());

                let resp = match bin_path {
                    Some(path) => match update_agent_binary(&path, &zstd_data, raw_len, &blake3) {
                        Ok(()) => Message::UpdateAgentResponse {
                            ok: true,
                            error: None,
                        },
                        Err(e) => Message::UpdateAgentResponse {
                            ok: false,
                            error: Some(e.to_string()),
                        },
                    },
                    None => Message::UpdateAgentResponse {
                        ok: false,
                        error: Some("Unable to determine agent binary path to update".to_string()),
                    },
                };
                write_message(writer, &resp)?;
            }

            Message::LockWorktreeRequest {
                client_id,
                repo,
                worktree,
            } => {
                // Validate path components to prevent escaping base_dir
                let is_safe = is_safe_relative_path(&client_id).is_ok()
                    && is_safe_relative_path(&repo).is_ok()
                    && is_safe_relative_path(&worktree).is_ok();

                if !is_safe {
                    let resp = Message::LockWorktreeResponse {
                        ok: false,
                        assigned_path: String::new(),
                        error: Some("Invalid client, repo, or worktree identifier".to_string()),
                    };
                    write_message(writer, &resp)?;
                    return Ok(true);
                }

                let wt_path = self.base_dir.join(&client_id).join(&repo).join(&worktree);
                match WorktreeLock::acquire(&wt_path) {
                    Ok(lock) => {
                        self.current_worktree = Some(wt_path.clone());
                        self.active_lock = Some(lock);
                        let resp = Message::LockWorktreeResponse {
                            ok: true,
                            assigned_path: wt_path.to_string_lossy().to_string(),
                            error: None,
                        };
                        write_message(writer, &resp)?;
                    }
                    Err(e) => {
                        let resp = Message::LockWorktreeResponse {
                            ok: false,
                            assigned_path: String::new(),
                            error: Some(e.to_string()),
                        };
                        write_message(writer, &resp)?;
                    }
                }
            }

            Message::SyncManifestRequest { entries } => {
                let wt = match &self.current_worktree {
                    Some(w) => w,
                    None => {
                        let resp = Message::ErrorResponse {
                            code: "NO_ACTIVE_WORKTREE".to_string(),
                            message: "Worktree lock has not been acquired".to_string(),
                        };
                        write_message(writer, &resp)?;
                        return Ok(true);
                    }
                };

                match calculate_diff(wt, &entries) {
                    Ok(diff) => {
                        self.pending_deletions = diff.deleted_files.clone();
                        self.updated_count = 0;
                        self.deleted_count = 0;
                        let resp = Message::SyncManifestResponse {
                            needed_files: diff.needed_files,
                            deleted_files: diff.deleted_files,
                        };
                        write_message(writer, &resp)?;
                    }
                    Err(e) => {
                        let resp = Message::ErrorResponse {
                            code: "DIFF_ERROR".to_string(),
                            message: e.to_string(),
                        };
                        write_message(writer, &resp)?;
                    }
                }
            }

            Message::SyncFileChunk(chunk) => {
                let wt = match &self.current_worktree {
                    Some(w) => w,
                    None => {
                        let resp = Message::SyncFileResponse {
                            path: chunk.path,
                            ok: false,
                            error: Some("No active worktree locked".to_string()),
                        };
                        write_message(writer, &resp)?;
                        return Ok(true);
                    }
                };

                let path = chunk.path.clone();
                match apply_file_chunk(wt, &chunk) {
                    Ok(()) => {
                        self.updated_count += 1;
                        let resp = Message::SyncFileResponse {
                            path,
                            ok: true,
                            error: None,
                        };
                        write_message(writer, &resp)?;
                    }
                    Err(e) => {
                        let resp = Message::SyncFileResponse {
                            path,
                            ok: false,
                            error: Some(e.to_string()),
                        };
                        write_message(writer, &resp)?;
                    }
                }
            }

            Message::SyncCompleteRequest => {
                let wt = match &self.current_worktree {
                    Some(w) => w,
                    None => {
                        let resp = Message::SyncCompleteResponse {
                            ok: false,
                            files_updated: 0,
                            files_deleted: 0,
                            error: Some("No active worktree locked".to_string()),
                        };
                        write_message(writer, &resp)?;
                        return Ok(true);
                    }
                };

                let del_count = match apply_deletions(wt, &self.pending_deletions) {
                    Ok(c) => c,
                    Err(e) => {
                        let resp = Message::SyncCompleteResponse {
                            ok: false,
                            files_updated: self.updated_count,
                            files_deleted: 0,
                            error: Some(e.to_string()),
                        };
                        write_message(writer, &resp)?;
                        return Ok(true);
                    }
                };
                self.deleted_count = del_count;
                self.pending_deletions.clear();

                let resp = Message::SyncCompleteResponse {
                    ok: true,
                    files_updated: self.updated_count,
                    files_deleted: self.deleted_count,
                    error: None,
                };
                write_message(writer, &resp)?;
            }

            Message::RunCommandRequest {
                command,
                args,
                envs,
            } => {
                let wt = match &self.current_worktree {
                    Some(w) => w.clone(),
                    None => {
                        let resp = Message::ErrorResponse {
                            code: "NO_ACTIVE_WORKTREE".to_string(),
                            message: "Worktree lock has not been acquired".to_string(),
                        };
                        write_message(writer, &resp)?;
                        return Ok(true);
                    }
                };

                let mut cmd = Command::new(&command);
                cmd.args(&args).current_dir(&wt);
                for (k, v) in envs {
                    cmd.env(k, v);
                }
                cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

                let mut child = match cmd.spawn() {
                    Ok(c) => c,
                    Err(e) => {
                        let resp = Message::CommandStderr {
                            data: format!("Failed to spawn command '{}': {}\n", command, e)
                                .into_bytes(),
                        };
                        write_message(writer, &resp)?;
                        write_message(writer, &Message::CommandExit { exit_code: -1 })?;
                        return Ok(true);
                    }
                };

                enum OutputEvent {
                    Stdout(Vec<u8>),
                    Stderr(Vec<u8>),
                }

                let (tx, rx) = std::sync::mpsc::channel();

                let mut stdout_handle = None;
                if let Some(mut out) = child.stdout.take() {
                    let tx_out = tx.clone();
                    stdout_handle = Some(thread::spawn(move || {
                        let mut buf = [0u8; 8192];
                        while let Ok(n) = out.read(&mut buf) {
                            if n == 0 {
                                break;
                            }
                            if tx_out.send(OutputEvent::Stdout(buf[..n].to_vec())).is_err() {
                                break;
                            }
                        }
                    }));
                }

                let mut stderr_handle = None;
                if let Some(mut err) = child.stderr.take() {
                    let tx_err = tx.clone();
                    stderr_handle = Some(thread::spawn(move || {
                        let mut buf = [0u8; 8192];
                        while let Ok(n) = err.read(&mut buf) {
                            if n == 0 {
                                break;
                            }
                            if tx_err.send(OutputEvent::Stderr(buf[..n].to_vec())).is_err() {
                                break;
                            }
                        }
                    }));
                }

                drop(tx); // Close the original transmitter so rx terminates when threads finish

                for event in rx {
                    match event {
                        OutputEvent::Stdout(data) => {
                            write_message(writer, &Message::CommandStdout { data })?;
                        }
                        OutputEvent::Stderr(data) => {
                            write_message(writer, &Message::CommandStderr { data })?;
                        }
                    }
                }

                if let Some(h) = stdout_handle {
                    let _ = h.join();
                }
                if let Some(h) = stderr_handle {
                    let _ = h.join();
                }

                let exit_code = match child.wait() {
                    Ok(status) => status.code().unwrap_or(-1),
                    Err(_) => -1,
                };
                write_message(writer, &Message::CommandExit { exit_code })?;
            }

            Message::CollectArtifactsRequest {
                command,
                targets,
                flags,
            } => {
                let wt = match &self.current_worktree {
                    Some(w) => w.clone(),
                    None => {
                        let resp = Message::CollectArtifactsResponse {
                            ok: false,
                            artifact_count: 0,
                            error: Some("Worktree lock has not been acquired".to_string()),
                        };
                        write_message(writer, &resp)?;
                        return Ok(true);
                    }
                };

                let bin = command.unwrap_or_else(|| "bazelisk".to_string());
                let mut cmd = Command::new(&bin);
                cmd.arg("cquery")
                    .arg("--output=files")
                    .args(&targets)
                    .args(&flags)
                    .current_dir(&wt);

                let output = match cmd.output() {
                    Ok(out) => out,
                    Err(e) => {
                        let resp = Message::CollectArtifactsResponse {
                            ok: false,
                            artifact_count: 0,
                            error: Some(format!("Failed to execute cquery with '{}': {}", bin, e)),
                        };
                        write_message(writer, &resp)?;
                        return Ok(true);
                    }
                };

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let resp = Message::CollectArtifactsResponse {
                        ok: false,
                        artifact_count: 0,
                        error: Some(format!(
                            "cquery exited with status {}: {}",
                            output.status,
                            stderr.trim()
                        )),
                    };
                    write_message(writer, &resp)?;
                    return Ok(true);
                }

                let stdout_str = String::from_utf8_lossy(&output.stdout);
                let mut artifact_count = 0;

                for line in stdout_str.lines() {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    let (rel_path, actual_path) = match resolve_artifact_file(&wt, trimmed) {
                        Ok(res) => res,
                        Err(e) => {
                            let resp = Message::CollectArtifactsResponse {
                                ok: false,
                                artifact_count,
                                error: Some(format!(
                                    "Failed to resolve artifact '{}': {}",
                                    trimmed, e
                                )),
                            };
                            write_message(writer, &resp)?;
                            return Ok(true);
                        }
                    };

                    let chunk = match create_artifact_chunk(&rel_path, &actual_path) {
                        Ok(c) => c,
                        Err(e) => {
                            let resp = Message::CollectArtifactsResponse {
                                ok: false,
                                artifact_count,
                                error: Some(format!(
                                    "Failed to read artifact '{}': {}",
                                    rel_path, e
                                )),
                            };
                            write_message(writer, &resp)?;
                            return Ok(true);
                        }
                    };

                    let chunk_msg = Message::ArtifactChunk(chunk);
                    let serialized_len = serde_json::to_vec(&chunk_msg)
                        .map_err(|e| anyhow::anyhow!("Failed to serialize artifact chunk: {}", e))?
                        .len() as u64;
                    if serialized_len > crate::protocol::frame::MAX_FRAME_SIZE as u64 {
                        let resp = Message::CollectArtifactsResponse {
                            ok: false,
                            artifact_count,
                            error: Some(format!(
                                "Artifact '{}' frame payload ({} bytes) exceeds protocol MAX_FRAME_SIZE ({} bytes)",
                                rel_path,
                                serialized_len,
                                crate::protocol::frame::MAX_FRAME_SIZE
                            )),
                        };
                        write_message(writer, &resp)?;
                        return Ok(true);
                    }

                    write_message(writer, &chunk_msg)?;
                    artifact_count += 1;
                }

                write_message(
                    writer,
                    &Message::CollectArtifactsResponse {
                        ok: true,
                        artifact_count,
                        error: None,
                    },
                )?;
            }

            Message::Ping => {
                write_message(writer, &Message::Pong)?;
            }

            _ => {
                // Unexpected or unhandled message
                let resp = Message::ErrorResponse {
                    code: "UNEXPECTED_MESSAGE".to_string(),
                    message: "Unexpected message received by agent".to_string(),
                };
                write_message(writer, &resp)?;
            }
        }

        Ok(true)
    }
}

/// Runs the agent event loop over reader and writer streams.
pub fn run_agent_loop<R: Read, W: Write>(
    mut reader: R,
    mut writer: W,
    base_dir: PathBuf,
    agent_bin_path: Option<PathBuf>,
) -> Result<()> {
    let mut session = AgentSession::new(base_dir, agent_bin_path);

    while let Some(msg) = read_message::<_, Message>(&mut reader)? {
        let should_continue = session.handle_message(&mut writer, msg)?;
        if !should_continue {
            break;
        }
    }

    Ok(())
}

/// Maximum supported artifact file size (16 MiB) to ensure single-frame delivery
/// within the 64 MiB protocol frame limit (considering serde_json byte array expansion).
pub const MAX_ARTIFACT_SIZE: u64 = 16 * 1024 * 1024;

fn resolve_artifact_file(worktree: &Path, raw_path_str: &str) -> Result<(String, PathBuf)> {
    let clean_str = raw_path_str.trim().trim_start_matches("./");
    if clean_str.is_empty() {
        anyhow::bail!("Empty artifact path in cquery output");
    }

    // Determine the bazel-bin-relative path
    let rel_under_bin: String = if let Some(stripped) = clean_str.strip_prefix("bazel-bin/") {
        stripped.to_string()
    } else if let Some(idx) = clean_str.find("/bin/") {
        clean_str[idx + 5..].to_string()
    } else if let Some(stripped) = clean_str.strip_prefix("bin/") {
        stripped.to_string()
    } else {
        clean_str.to_string()
    };

    // Enforce path safety
    is_safe_relative_path(&rel_under_bin)
        .map_err(|e| anyhow::anyhow!("Unsafe artifact path '{}': {}", rel_under_bin, e))?;

    // Find actual on-disk file
    let candidate_a = worktree.join(clean_str);
    let candidate_b = worktree.join("bazel-bin").join(&rel_under_bin);
    let candidate_c = worktree.join(&rel_under_bin);

    let actual_path = if candidate_a.exists() || fs::symlink_metadata(&candidate_a).is_ok() {
        candidate_a
    } else if candidate_b.exists() || fs::symlink_metadata(&candidate_b).is_ok() {
        candidate_b
    } else if candidate_c.exists() || fs::symlink_metadata(&candidate_c).is_ok() {
        candidate_c
    } else {
        anyhow::bail!(
            "Artifact file not found on agent: '{}' (checked '{}', '{}', '{}')",
            raw_path_str,
            candidate_a.display(),
            candidate_b.display(),
            candidate_c.display()
        );
    };

    // Canonicalize check:
    // Ensure both worktree and actual_path are canonicalized, and actual_path is contained within worktree.
    let canonical_worktree = fs::canonicalize(worktree)
        .with_context(|| format!("Canonicalizing worktree '{}'", worktree.display()))?;
    let canonical_actual = fs::canonicalize(&actual_path).with_context(|| {
        format!(
            "Resolving canonical path for artifact '{}'",
            actual_path.display()
        )
    })?;

    if !canonical_actual.starts_with(&canonical_worktree) {
        anyhow::bail!(
            "Artifact path '{}' escapes worktree '{}' (resolved to '{}')",
            raw_path_str,
            worktree.display(),
            canonical_actual.display()
        );
    }

    Ok((rel_under_bin, actual_path))
}

fn create_artifact_chunk(rel_path: &str, actual_path: &Path) -> Result<SyncFileChunk> {
    let meta = fs::symlink_metadata(actual_path)?;
    let is_symlink = meta.file_type().is_symlink();
    let mode = meta.permissions().mode();

    if is_symlink {
        let symlink_target = fs::read_link(actual_path)?.to_string_lossy().to_string();
        let blake3_hash = blake3::hash(symlink_target.as_bytes()).to_hex().to_string();
        Ok(SyncFileChunk {
            path: rel_path.to_string(),
            mode,
            is_symlink: true,
            symlink_target: Some(symlink_target),
            raw_len: 0,
            blake3: blake3_hash,
            zstd_data: Vec::new(),
        })
    } else {
        let file_len = meta.len();
        if file_len > MAX_ARTIFACT_SIZE {
            anyhow::bail!(
                "Artifact '{}' size ({} bytes) exceeds maximum supported limit ({} bytes)",
                rel_path,
                file_len,
                MAX_ARTIFACT_SIZE
            );
        }

        let bytes = fs::read(actual_path)?;
        let raw_len = bytes.len() as u64;
        let blake3_hash = blake3::hash(&bytes).to_hex().to_string();
        let zstd_data = zstd::encode_all(&bytes[..], 3)?;
        Ok(SyncFileChunk {
            path: rel_path.to_string(),
            mode,
            is_symlink: false,
            symlink_target: None,
            raw_len,
            blake3: blake3_hash,
            zstd_data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_artifact_file_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path();

        // 1. bazel-bin layout
        let bin_dir = wt.join("bazel-bin").join("pkg");
        fs::create_dir_all(&bin_dir).unwrap();
        let artifact = bin_dir.join("app");
        fs::write(&artifact, b"binary").unwrap();

        let (rel, path) = resolve_artifact_file(wt, "bazel-bin/pkg/app").unwrap();
        assert_eq!(rel, "pkg/app");
        assert_eq!(path, artifact);

        // 2. bazel-out layout
        let out_dir = wt.join("bazel-out/darwin_arm64-fastbuild/bin/pkg");
        fs::create_dir_all(&out_dir).unwrap();
        let artifact2 = out_dir.join("tool");
        fs::write(&artifact2, b"tool_data").unwrap();

        let (rel2, path2) =
            resolve_artifact_file(wt, "bazel-out/darwin_arm64-fastbuild/bin/pkg/tool").unwrap();
        assert_eq!(rel2, "pkg/tool");
        assert_eq!(path2, artifact2);
    }

    #[test]
    fn test_resolve_artifact_file_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path();
        let res = resolve_artifact_file(wt, "bazel-bin/../../../etc/passwd");
        assert!(
            res.is_err(),
            "Must reject path traversal in artifact output"
        );
    }

    #[test]
    fn test_resolve_artifact_file_rejects_symlink_escape() {
        let wt_dir = tempfile::tempdir().unwrap();
        let wt = wt_dir.path();

        let outside_dir = tempfile::tempdir().unwrap();
        let outside_file = outside_dir.path().join("secret.txt");
        fs::write(&outside_file, b"secret_data").unwrap();

        // Create symlink inside worktree pointing outside
        let bin_pkg = wt.join("bazel-bin/pkg");
        fs::create_dir_all(&bin_pkg).unwrap();
        let escape_link = bin_pkg.join("leak_link");
        std::os::unix::fs::symlink(&outside_file, &escape_link).unwrap();

        let res = resolve_artifact_file(wt, "bazel-bin/pkg/leak_link");
        assert!(
            res.is_err(),
            "Must reject symlink escaping worktree root in artifact mapping"
        );
        let err_msg = res.unwrap_err().to_string();
        assert!(
            err_msg.contains("escapes worktree"),
            "Error message must indicate worktree escape: {}",
            err_msg
        );
    }

    #[test]
    fn test_create_artifact_chunk_rejects_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let large_file = dir.path().join("large_binary");
        let file = fs::File::create(&large_file).unwrap();
        // Allocate 1 byte beyond MAX_ARTIFACT_SIZE
        file.set_len(MAX_ARTIFACT_SIZE + 1).unwrap();

        let res = create_artifact_chunk("pkg/large_binary", &large_file);
        assert!(
            res.is_err(),
            "Must reject artifacts exceeding MAX_ARTIFACT_SIZE"
        );
        let err_msg = res.unwrap_err().to_string();
        assert!(
            err_msg.contains("exceeds maximum supported limit"),
            "Error message must specify size limit: {}",
            err_msg
        );
    }
}
