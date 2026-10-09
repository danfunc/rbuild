pub mod transport;

use crate::client::transport::Transport;
use crate::protocol::message::{ManifestEntry, Message, SyncFileChunk};
use crate::sync::apply::apply_file_chunk;
use crate::sync::manifest::{ManifestCache, build_manifest};
use crate::sync::path_safety::resolve_safe_path;
use crate::version::{PROTOCOL_VERSION, get_current_build_id};
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
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

struct ChunkedArtifact {
    path: String,
    mode: u32,
    raw_len: u64,
    expected_hash: String,
    written: u64,
    hasher: blake3::Hasher,
    temp: tempfile::NamedTempFile,
    target: std::path::PathBuf,
}

fn handle_artifact_transfer(
    output_dir: &Path,
    pending: &mut Option<ChunkedArtifact>,
    count: &mut usize,
    message: Message,
) -> Result<Option<Message>> {
    match message {
        Message::ArtifactStart {
            path,
            mode,
            raw_len,
            blake3,
        } => {
            if pending.is_some() {
                bail!("Received overlapping artifact transfers");
            }
            let target = resolve_safe_path(output_dir, &path)
                .map_err(|e| anyhow::anyhow!("Unsafe artifact path: {e}"))?;
            let parent = target
                .parent()
                .ok_or_else(|| anyhow::anyhow!("Artifact path has no parent"))?;
            fs::create_dir_all(parent)?;
            let temp = tempfile::Builder::new()
                .prefix(".rbuild-artifact-")
                .tempfile_in(parent)?;
            *pending = Some(ChunkedArtifact {
                path,
                mode,
                raw_len,
                expected_hash: blake3,
                written: 0,
                hasher: blake3::Hasher::new(),
                temp,
                target,
            });
            Ok(None)
        }
        Message::ArtifactData {
            path,
            offset,
            raw_len,
            zstd_data,
        } => {
            let state = pending
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("ArtifactData received without ArtifactStart"))?;
            if path != state.path || offset != state.written {
                bail!("Out-of-order or mismatched artifact chunk for {path}");
            }
            let bytes = zstd::decode_all(&zstd_data[..])?;
            if bytes.len() as u64 != raw_len {
                bail!("Artifact chunk length mismatch for {path}");
            }
            state.temp.write_all(&bytes)?;
            state.hasher.update(&bytes);
            state.written += raw_len;
            if state.written > state.raw_len {
                bail!("Artifact exceeds declared length: {path}");
            }
            Ok(None)
        }
        Message::ArtifactEnd { path } => {
            let state = pending
                .take()
                .ok_or_else(|| anyhow::anyhow!("ArtifactEnd received without ArtifactStart"))?;
            if path != state.path || state.written != state.raw_len {
                bail!("Artifact transfer incomplete for {path}");
            }
            let actual_hash = state.hasher.finalize().to_hex().to_string();
            if actual_hash != state.expected_hash {
                bail!("Artifact BLAKE3 mismatch for {path}");
            }
            state.temp.as_file().set_len(state.raw_len)?;
            state.temp.as_file().sync_all()?;
            fs::set_permissions(state.temp.path(), fs::Permissions::from_mode(state.mode))?;
            state
                .temp
                .persist(&state.target)
                .with_context(|| format!("Persisting artifact {}", state.path))?;
            *count += 1;
            Ok(None)
        }
        other => Ok(Some(other)),
    }
}

impl<T: Transport> ClientSession<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Performs handshake, verifying protocol version and reporting platform identity.
    /// If incompatible and on the same platform with local binary accessible, initiates agent update.
    /// Refuses to send local macOS binary to a Linux agent.
    pub fn handshake(&mut self, local_exe_path: Option<&Path>) -> Result<()> {
        let local_build_id = get_current_build_id();
        let local_platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);

        self.transport.send(&Message::HandshakeRequest {
            protocol_version: PROTOCOL_VERSION,
            build_id: local_build_id.clone(),
            platform: Some(local_platform.clone()),
        })?;

        match self.transport.recv()? {
            Message::HandshakeResponse {
                protocol_version: remote_protocol,
                compatible,
                build_id: remote_build_id,
                platform: remote_platform,
                message,
            } => {
                let remote_plat = remote_platform.as_deref().unwrap_or("unknown");

                if compatible && remote_protocol == PROTOCOL_VERSION {
                    // Different build IDs across different platforms (e.g. macOS client vs Linux agent) are expected and normal.
                    if local_build_id != remote_build_id {
                        eprintln!(
                            "rbuild: Note: Build ID differs between client ({}) and agent ({}). Remote platform is '{}'. Protocol v{} is compatible.",
                            local_build_id, remote_build_id, remote_plat, PROTOCOL_VERSION
                        );
                    }
                    return Ok(());
                }
                if compatible {
                    bail!(
                        "Remote agent reported compatibility for protocol v{} but this client speaks v{}. Refusing to continue.",
                        remote_protocol,
                        PROTOCOL_VERSION
                    );
                }

                let reason = message.unwrap_or_else(|| "Protocol mismatch".to_string());
                if remote_plat != local_platform {
                    bail!(
                        "Remote agent is incompatible ({}). Cannot self-update cross-platform agent: \
                        remote platform is '{}' while local host is '{}'. Sending a local binary would corrupt \
                        the remote host. Please deploy a native-built agent binary on the remote host (e.g. via 'cargo build --release').",
                        reason,
                        remote_plat,
                        local_platform
                    );
                }

                let Some(exe) = local_exe_path else {
                    bail!(
                        "Remote agent is incompatible ({}): it speaks protocol v{}, this client speaks v{}. \
                        No local binary is available to deploy.",
                        reason,
                        remote_protocol,
                        PROTOCOL_VERSION
                    );
                };
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
                    // The running agent process keeps the old code after its binary is replaced,
                    // so this session must not proceed. The next connection starts the new binary.
                    Message::UpdateAgentResponse { ok: true, .. } => bail!(
                        "Remote agent binary was updated to protocol v{}. The running agent is still the old process: \
                        reconnect and retry. No worktree was locked and nothing was built.",
                        PROTOCOL_VERSION
                    ),
                    Message::UpdateAgentResponse { ok: false, error } => {
                        bail!("Failed to update remote agent binary: {:?}", error);
                    }
                    other => bail!("Unexpected response to UpdateAgentRequest: {:?}", other),
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

    /// Runs a remote command, forwards output, and returns captured stdout/stderr.
    pub fn run_remote_command_capture(
        &mut self,
        command: &str,
        args: &[String],
        envs: &[(String, String)],
    ) -> Result<(i32, Vec<u8>, Vec<u8>)> {
        self.transport.send(&Message::RunCommandRequest {
            command: command.to_string(),
            args: args.to_vec(),
            envs: envs.to_vec(),
        })?;
        let mut out = Vec::new();
        let mut err = Vec::new();
        loop {
            match self.transport.recv()? {
                Message::CommandStdout { data } => {
                    std::io::stdout().write_all(&data)?;
                    std::io::stdout().flush()?;
                    out.extend_from_slice(&data);
                }
                Message::CommandStderr { data } => {
                    std::io::stderr().write_all(&data)?;
                    std::io::stderr().flush()?;
                    err.extend_from_slice(&data);
                }
                Message::CommandExit { exit_code } => return Ok((exit_code, out, err)),
                Message::ErrorResponse { code, message } => {
                    bail!("Agent reported command error [{}]: {}", code, message)
                }
                other => bail!("Unexpected response during command execution: {:?}", other),
            }
        }
    }

    /// Copies validated worktree relative paths into the local output directory.
    pub fn collect_paths(&mut self, output_dir: &Path, paths: &[String]) -> Result<usize> {
        fs::create_dir_all(output_dir)?;
        self.transport.send(&Message::CollectPathsRequest {
            paths: paths.to_vec(),
        })?;
        let mut count = 0usize;
        let mut pending = None;
        loop {
            let message = self.transport.recv()?;
            let Some(message) =
                handle_artifact_transfer(output_dir, &mut pending, &mut count, message)?
            else {
                continue;
            };
            match message {
                Message::ArtifactChunk(chunk) => {
                    apply_file_chunk(output_dir, &chunk)?;
                    count += 1;
                }
                Message::CollectPathsResponse {
                    ok: true,
                    artifact_count,
                    ..
                } => {
                    if pending.is_some() {
                        bail!("Agent reported success while an artifact transfer was unfinished");
                    }
                    if count != artifact_count {
                        bail!(
                            "Artifact count mismatch: received {count}, agent reported {artifact_count}"
                        );
                    }
                    return Ok(count);
                }
                Message::CollectPathsResponse {
                    ok: false, error, ..
                } => bail!("Artifact collection failed: {}", error.unwrap_or_default()),
                other => bail!("Unexpected response while collecting paths: {:?}", other),
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
        self.collect_artifacts_with_envs(output_dir, command_override, targets, flags, &[])
    }

    /// Same as [`Self::collect_artifacts`], with extra environment entries for the remote cquery.
    pub fn collect_artifacts_with_envs(
        &mut self,
        output_dir: &Path,
        command_override: Option<&str>,
        targets: &[String],
        flags: &[String],
        envs: &[(String, String)],
    ) -> Result<usize> {
        fs::create_dir_all(output_dir)
            .with_context(|| format!("Creating output directory: {}", output_dir.display()))?;

        self.transport.send(&Message::CollectArtifactsRequest {
            command: command_override.map(|s| s.to_string()),
            targets: targets.to_vec(),
            flags: flags.to_vec(),
            envs: envs.to_vec(),
        })?;

        let mut collected = 0;
        let mut pending = None;
        loop {
            let message = self.transport.recv()?;
            let Some(message) =
                handle_artifact_transfer(output_dir, &mut pending, &mut collected, message)?
            else {
                continue;
            };
            match message {
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
                    if pending.is_some() {
                        bail!("Agent reported success while an artifact transfer was unfinished");
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct MockTransport {
        sent: Vec<Message>,
        responses: VecDeque<Message>,
    }

    impl MockTransport {
        fn new(responses: Vec<Message>) -> Self {
            Self {
                sent: Vec::new(),
                responses: responses.into(),
            }
        }
    }

    impl Transport for MockTransport {
        fn send(&mut self, msg: &Message) -> Result<()> {
            self.sent.push(msg.clone());
            Ok(())
        }

        fn recv(&mut self) -> Result<Message> {
            self.responses
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("No more mock responses"))
        }
    }

    #[test]
    fn test_client_handshake_cross_platform_compatible() {
        let resp = Message::HandshakeResponse {
            protocol_version: PROTOCOL_VERSION,
            build_id: "linux-different-build-id".to_string(),
            compatible: true,
            platform: Some("linux-x86_64".to_string()),
            message: None,
        };

        let transport = MockTransport::new(vec![resp]);
        let mut session = ClientSession::new(transport);

        let res = session.handshake(None);
        assert!(
            res.is_ok(),
            "Compatible protocol must succeed across platforms"
        );
        assert_eq!(session.transport.sent.len(), 1);
        match &session.transport.sent[0] {
            Message::HandshakeRequest {
                protocol_version,
                platform,
                ..
            } => {
                assert_eq!(*protocol_version, PROTOCOL_VERSION);
                assert!(platform.is_some());
            }
            other => panic!("Expected HandshakeRequest, got {:?}", other),
        }
    }

    #[test]
    fn test_client_handshake_incompatible_cross_platform_fails_closed() {
        let resp = Message::HandshakeResponse {
            protocol_version: PROTOCOL_VERSION + 1,
            build_id: "linux-build-id".to_string(),
            compatible: false,
            platform: Some("different_os-unknown_arch".to_string()),
            message: Some("Protocol version mismatch".to_string()),
        };

        let transport = MockTransport::new(vec![resp]);
        let mut session = ClientSession::new(transport);

        let fake_exe = Path::new("/tmp/fake_macos_binary");
        let res = session.handshake(Some(fake_exe));

        assert!(
            res.is_err(),
            "Must fail closed without sending binary to mismatched OS"
        );
        let err_msg = res.unwrap_err().to_string();
        assert!(
            err_msg.contains("Cannot self-update cross-platform agent"),
            "Error must explicitly explain cross-platform update refusal: {}",
            err_msg
        );
        // Ensure no update request was sent
        assert_eq!(
            session.transport.sent.len(),
            1,
            "Must not send UpdateAgentRequest"
        );
    }

    fn local_platform() -> String {
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
    }

    fn handshake_response(protocol_version: u32, compatible: bool, platform: &str) -> Message {
        Message::HandshakeResponse {
            protocol_version,
            build_id: "remote-build-id".to_string(),
            compatible,
            platform: Some(platform.to_string()),
            message: (!compatible).then(|| "Protocol version mismatch".to_string()),
        }
    }

    #[test]
    fn test_handshake_old_protocol_without_binary_fails_before_lock() {
        let transport = MockTransport::new(vec![handshake_response(1, false, &local_platform())]);
        let mut session = ClientSession::new(transport);

        let err = session.handshake(None).unwrap_err().to_string();
        assert!(err.contains("No local binary"), "{err}");
        assert_eq!(
            session.transport.sent.len(),
            1,
            "Only HandshakeRequest may be sent; no update, lock, or build"
        );
    }

    #[test]
    fn test_handshake_cross_platform_old_protocol_sends_no_binary() {
        let transport = MockTransport::new(vec![handshake_response(1, false, "linux-x86_64")]);
        let mut session = ClientSession::new(transport);

        let exe = tempfile::NamedTempFile::new().unwrap();
        fs::write(exe.path(), b"client binary").unwrap();
        let err = session.handshake(Some(exe.path())).unwrap_err().to_string();
        assert!(
            err.contains("Cannot self-update cross-platform agent"),
            "{err}"
        );
        assert_eq!(session.transport.sent.len(), 1);
        assert!(
            !session
                .transport
                .sent
                .iter()
                .any(|m| matches!(m, Message::UpdateAgentRequest { .. })),
            "Binary must not be sent to a different platform"
        );
    }

    #[test]
    fn test_handshake_same_platform_update_requires_reconnect() {
        let transport = MockTransport::new(vec![
            handshake_response(1, false, &local_platform()),
            Message::UpdateAgentResponse {
                ok: true,
                error: None,
            },
        ]);
        let mut session = ClientSession::new(transport);

        let exe = tempfile::NamedTempFile::new().unwrap();
        fs::write(exe.path(), b"client binary").unwrap();
        let err = session.handshake(Some(exe.path())).unwrap_err().to_string();
        assert!(err.contains("reconnect and retry"), "{err}");
        assert_eq!(session.transport.sent.len(), 2);
        assert!(matches!(
            session.transport.sent[1],
            Message::UpdateAgentRequest { .. }
        ));
        assert!(
            !session
                .transport
                .sent
                .iter()
                .any(|m| matches!(m, Message::LockWorktreeRequest { .. })),
            "No lock may be requested after an update"
        );
    }

    #[test]
    fn test_handshake_same_platform_update_failure_is_reported() {
        let transport = MockTransport::new(vec![
            handshake_response(1, false, &local_platform()),
            Message::UpdateAgentResponse {
                ok: false,
                error: Some("disk full".to_string()),
            },
        ]);
        let mut session = ClientSession::new(transport);

        let exe = tempfile::NamedTempFile::new().unwrap();
        fs::write(exe.path(), b"client binary").unwrap();
        let err = session.handshake(Some(exe.path())).unwrap_err().to_string();
        assert!(
            err.contains("Failed to update remote agent binary"),
            "{err}"
        );
        assert!(err.contains("disk full"), "{err}");
    }

    #[test]
    fn test_handshake_new_protocol_passes_despite_build_id_difference() {
        let transport = MockTransport::new(vec![handshake_response(
            PROTOCOL_VERSION,
            true,
            "linux-x86_64",
        )]);
        let mut session = ClientSession::new(transport);

        session.handshake(None).expect("same protocol must pass");
        assert_eq!(session.transport.sent.len(), 1);
    }

    #[test]
    fn test_handshake_rejects_compatible_claim_with_other_protocol() {
        let transport = MockTransport::new(vec![handshake_response(1, true, &local_platform())]);
        let mut session = ClientSession::new(transport);

        let err = session.handshake(None).unwrap_err().to_string();
        assert!(err.contains("protocol v1"), "{err}");
    }

    fn feed(
        dir: &Path,
        pending: &mut Option<ChunkedArtifact>,
        count: &mut usize,
        message: Message,
    ) -> Result<Option<Message>> {
        handle_artifact_transfer(dir, pending, count, message)
    }

    fn data_frame(path: &str, offset: u64, bytes: &[u8]) -> Message {
        Message::ArtifactData {
            path: path.to_string(),
            offset,
            raw_len: bytes.len() as u64,
            zstd_data: zstd::encode_all(bytes, 3).unwrap(),
        }
    }

    #[test]
    fn test_artifact_chunks_reassemble_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let data = b"0123456789abcdef";
        let mut pending = None;
        let mut count = 0;
        let start = Message::ArtifactStart {
            path: "bin/app".to_string(),
            mode: 0o755,
            raw_len: data.len() as u64,
            blake3: blake3::hash(data).to_hex().to_string(),
        };
        assert!(
            feed(dir.path(), &mut pending, &mut count, start)
                .unwrap()
                .is_none()
        );
        feed(
            dir.path(),
            &mut pending,
            &mut count,
            data_frame("bin/app", 0, &data[..8]),
        )
        .unwrap();
        feed(
            dir.path(),
            &mut pending,
            &mut count,
            data_frame("bin/app", 8, &data[8..]),
        )
        .unwrap();
        feed(
            dir.path(),
            &mut pending,
            &mut count,
            Message::ArtifactEnd {
                path: "bin/app".to_string(),
            },
        )
        .unwrap();

        assert_eq!(count, 1);
        assert!(pending.is_none());
        assert_eq!(fs::read(dir.path().join("bin/app")).unwrap(), data);
    }

    #[test]
    fn test_artifact_out_of_order_chunk_rejected_without_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut pending = None;
        let mut count = 0;
        let start = Message::ArtifactStart {
            path: "app".to_string(),
            mode: 0o644,
            raw_len: 16,
            blake3: "00".repeat(32),
        };
        feed(dir.path(), &mut pending, &mut count, start).unwrap();
        let err = feed(
            dir.path(),
            &mut pending,
            &mut count,
            data_frame("app", 4, b"xxxx"),
        )
        .err()
        .expect("gap in offsets must be rejected");
        assert!(err.to_string().contains("Out-of-order"));
        assert!(!dir.path().join("app").exists());
    }

    #[test]
    fn test_artifact_hash_mismatch_leaves_no_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut pending = None;
        let mut count = 0;
        let start = Message::ArtifactStart {
            path: "app".to_string(),
            mode: 0o644,
            raw_len: 4,
            blake3: blake3::hash(b"good").to_hex().to_string(),
        };
        feed(dir.path(), &mut pending, &mut count, start).unwrap();
        feed(
            dir.path(),
            &mut pending,
            &mut count,
            data_frame("app", 0, b"evil"),
        )
        .unwrap();
        let err = feed(
            dir.path(),
            &mut pending,
            &mut count,
            Message::ArtifactEnd {
                path: "app".to_string(),
            },
        )
        .err()
        .expect("hash mismatch must be rejected");
        assert!(err.to_string().contains("BLAKE3 mismatch"));
        assert_eq!(count, 0);
        assert!(!dir.path().join("app").exists());
        let leftovers = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 0, "temporary artifact file must be removed");
    }

    #[test]
    fn test_artifact_start_rejects_escaping_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut pending = None;
        let mut count = 0;
        let start = Message::ArtifactStart {
            path: "../escape".to_string(),
            mode: 0o644,
            raw_len: 0,
            blake3: String::new(),
        };
        assert!(feed(dir.path(), &mut pending, &mut count, start).is_err());
        assert!(pending.is_none());
    }

    #[test]
    fn test_non_artifact_messages_pass_through() {
        let dir = tempfile::tempdir().unwrap();
        let mut pending = None;
        let mut count = 0;
        let passed = feed(dir.path(), &mut pending, &mut count, Message::Pong).unwrap();
        assert_eq!(passed, Some(Message::Pong));
    }
}
