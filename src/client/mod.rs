pub mod transport;

use crate::client::transport::Transport;
use crate::protocol::message::{ManifestEntry, Message, SyncFileChunk};
use crate::sync::apply::apply_file_chunk;
use crate::sync::manifest::{ManifestCache, build_manifest};
use crate::version::{PROTOCOL_VERSION, get_current_build_id};
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncStats {
    pub files_checked: usize,
    pub files_transferred: usize,
    pub files_deleted: usize,
}

pub struct ClientSession<T: Transport> {
    transport: T,
}

impl<T: Transport> ClientSession<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Performs handshake, verifying protocol version and build ID.
    /// If incompatible and local binary is accessible, initiates agent update.
    pub fn handshake(&mut self, local_exe_path: Option<&Path>) -> Result<()> {
        let local_build_id = get_current_build_id();
        self.transport.send(&Message::HandshakeRequest {
            protocol_version: PROTOCOL_VERSION,
            build_id: local_build_id.clone(),
        })?;

        match self.transport.recv()? {
            Message::HandshakeResponse {
                compatible,
                message,
                ..
            } => {
                if compatible {
                    return Ok(());
                }

                // If incompatible, attempt self-update if binary is available
                if let Some(exe) = local_exe_path {
                    let binary_data = fs::read(exe).with_context(|| {
                        format!(
                            "Reading local executable for agent update: {}",
                            exe.display()
                        )
                    })?;
                    let raw_len = binary_data.len() as u64;
                    let blake3_hash = blake3::hash(&binary_data).to_hex().to_string();
                    let zstd_data = zstd::encode_all(&binary_data[..], 3)?;

                    self.transport.send(&Message::UpdateAgentRequest {
                        build_id: local_build_id,
                        raw_len,
                        blake3: blake3_hash,
                        zstd_data,
                    })?;

                    match self.transport.recv()? {
                        Message::UpdateAgentResponse { ok: true, .. } => Ok(()),
                        Message::UpdateAgentResponse { ok: false, error } => {
                            bail!("Failed to update remote agent binary: {:?}", error);
                        }
                        other => bail!("Unexpected response to UpdateAgentRequest: {:?}", other),
                    }
                } else {
                    bail!("Remote agent is incompatible: {:?}", message);
                }
            }
            other => bail!("Unexpected handshake response: {:?}", other),
        }
    }

    /// Requests worktree allocation and exclusive locking on the agent.
    pub fn lock_worktree(&mut self, client_id: &str, repo: &str, worktree: &str) -> Result<String> {
        self.transport.send(&Message::LockWorktreeRequest {
            client_id: client_id.to_string(),
            repo: repo.to_string(),
            worktree: worktree.to_string(),
        })?;

        match self.transport.recv()? {
            Message::LockWorktreeResponse {
                ok: true,
                assigned_path,
                ..
            } => Ok(assigned_path),
            Message::LockWorktreeResponse {
                ok: false, error, ..
            } => {
                bail!("Failed to acquire remote worktree lock: {:?}", error);
            }
            other => bail!("Unexpected response to LockWorktreeRequest: {:?}", other),
        }
    }

    /// Synchronizes the local repository into the remote worktree.
    pub fn sync_workspace(
        &mut self,
        source_dir: &Path,
        cache: &mut ManifestCache,
        extra_excludes: &[String],
    ) -> Result<SyncStats> {
        let manifest = build_manifest(source_dir, cache, extra_excludes)?;
        let manifest_count = manifest.len();

        self.transport.send(&Message::SyncManifestRequest {
            entries: manifest.clone(),
        })?;

        let (needed_files, _deleted_files) = match self.transport.recv()? {
            Message::SyncManifestResponse {
                needed_files,
                deleted_files,
            } => (needed_files, deleted_files),
            other => bail!("Unexpected response to SyncManifestRequest: {:?}", other),
        };

        // Transfer all needed files
        let mut transferred = 0;
        let entry_map: std::collections::HashMap<&str, &ManifestEntry> =
            manifest.iter().map(|e| (e.path.as_str(), e)).collect();

        for rel_path in &needed_files {
            let entry = match entry_map.get(rel_path.as_str()) {
                Some(e) => *e,
                None => continue,
            };

            let local_path = source_dir.join(rel_path);
            let chunk = if entry.is_symlink {
                SyncFileChunk {
                    path: rel_path.clone(),
                    mode: entry.mode,
                    is_symlink: true,
                    symlink_target: entry.symlink_target.clone(),
                    raw_len: 0,
                    blake3: entry.blake3_hash.clone(),
                    zstd_data: Vec::new(),
                }
            } else {
                let bytes = fs::read(&local_path).with_context(|| {
                    format!("Reading file for transfer: {}", local_path.display())
                })?;
                let raw_len = bytes.len() as u64;
                let blake3_hash = blake3::hash(&bytes).to_hex().to_string();
                let zstd_data = zstd::encode_all(&bytes[..], 3)?;

                SyncFileChunk {
                    path: rel_path.clone(),
                    mode: entry.mode,
                    is_symlink: false,
                    symlink_target: None,
                    raw_len,
                    blake3: blake3_hash,
                    zstd_data,
                }
            };

            self.transport.send(&Message::SyncFileChunk(chunk))?;

            match self.transport.recv()? {
                Message::SyncFileResponse { ok: true, .. } => {
                    transferred += 1;
                }
                Message::SyncFileResponse {
                    ok: false, error, ..
                } => {
                    bail!("Agent failed to accept file '{}': {:?}", rel_path, error);
                }
                other => bail!("Unexpected response to SyncFileChunk: {:?}", other),
            }
        }

        // Notify sync completion to apply deletions and finalize
        self.transport.send(&Message::SyncCompleteRequest)?;

        let deleted_count = match self.transport.recv()? {
            Message::SyncCompleteResponse {
                ok: true,
                files_deleted,
                ..
            } => files_deleted,
            Message::SyncCompleteResponse {
                ok: false, error, ..
            } => {
                bail!("Agent failed to finalize sync: {:?}", error);
            }
            other => bail!("Unexpected response to SyncCompleteRequest: {:?}", other),
        };

        Ok(SyncStats {
            files_checked: manifest_count,
            files_transferred: transferred,
            files_deleted: deleted_count,
        })
    }

    /// Runs a remote command and forwards its stdout/stderr streams to standard output/error.
    pub fn run_remote_command(
        &mut self,
        command: &str,
        args: &[String],
        envs: &[(String, String)],
    ) -> Result<i32> {
        let mut stdout = std::io::stdout();
        let mut stderr = std::io::stderr();
        self.run_remote_command_to_writers(command, args, envs, &mut stdout, &mut stderr)
    }

    /// Runs a remote command and forwards its stdout/stderr streams to custom writers.
    pub fn run_remote_command_to_writers<W1: Write, W2: Write>(
        &mut self,
        command: &str,
        args: &[String],
        envs: &[(String, String)],
        stdout: &mut W1,
        stderr: &mut W2,
    ) -> Result<i32> {
        self.transport.send(&Message::RunCommandRequest {
            command: command.to_string(),
            args: args.to_vec(),
            envs: envs.to_vec(),
        })?;

        loop {
            match self.transport.recv()? {
                Message::CommandStdout { data } => {
                    stdout.write_all(&data)?;
                    stdout.flush()?;
                }
                Message::CommandStderr { data } => {
                    stderr.write_all(&data)?;
                    stderr.flush()?;
                }
                Message::CommandExit { exit_code } => {
                    return Ok(exit_code);
                }
                Message::ErrorResponse { code, message } => {
                    bail!("Agent reported command error [{}]: {}", code, message);
                }
                other => bail!("Unexpected response during command execution: {:?}", other),
            }
        }
    }

    /// Collects build artifacts from remote agent into the specified local directory (e.g. `.rbuild-out`).
    ///
    /// Validates path safety, verifying no files escape the local output directory.
    pub fn collect_artifacts(
        &mut self,
        output_dir: &Path,
        command_override: Option<&str>,
        targets: &[String],
        flags: &[String],
    ) -> Result<usize> {
        fs::create_dir_all(output_dir)
            .with_context(|| format!("Creating output directory: {}", output_dir.display()))?;

        self.transport.send(&Message::CollectArtifactsRequest {
            command: command_override.map(|s| s.to_string()),
            targets: targets.to_vec(),
            flags: flags.to_vec(),
        })?;

        let mut collected = 0;
        loop {
            match self.transport.recv()? {
                Message::ArtifactChunk(chunk) => {
                    apply_file_chunk(output_dir, &chunk).with_context(|| {
                        format!("Applying downloaded artifact chunk for '{}'", chunk.path)
                    })?;
                    collected += 1;
                }
                Message::CollectArtifactsResponse {
                    ok: true,
                    artifact_count,
                    ..
                } => {
                    if collected != artifact_count {
                        bail!(
                            "Mismatch between transferred artifact chunks ({}) and server reported count ({})",
                            collected,
                            artifact_count
                        );
                    }
                    return Ok(collected);
                }

                Message::CollectArtifactsResponse {
                    ok: false, error, ..
                } => {
                    bail!(
                        "Agent failed to query/collect build artifacts: {:?}",
                        error.unwrap_or_else(|| "Unknown error".to_string())
                    );
                }
                Message::ErrorResponse { code, message } => {
                    bail!("Agent reported artifact error [{}]: {}", code, message);
                }
                other => bail!(
                    "Unexpected response during artifact collection: {:?}",
                    other
                ),
            }
        }
    }
}
