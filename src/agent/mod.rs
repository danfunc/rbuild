pub mod lock;
pub mod updater;

use crate::agent::lock::WorktreeLock;
use crate::agent::updater::update_agent_binary;
use crate::protocol::message::{Message, PATH_PREPEND_ENV, SyncFileChunk};
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
    pub active_locks: Vec<WorktreeLock>,
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
            active_locks: Vec::new(),
            pending_deletions: Vec::new(),
            updated_count: 0,
            deleted_count: 0,
        }
    }

    pub fn handle_message<W: Write>(&mut self, writer: &mut W, msg: Message) -> Result<bool> {
        match msg {
            Message::HandshakeRequest {
                protocol_version,
                build_id: _remote_build_id,
                platform: _remote_platform,
            } => {
                let local_build_id = get_current_build_id();
                let local_platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
                // Protocol compatibility is decoupled from build ID.
                // Legitimate cross-platform agents (e.g. Linux agent vs macOS client) will have different build IDs.
                let compatible = protocol_version == PROTOCOL_VERSION;
                let response = Message::HandshakeResponse {
                    protocol_version: PROTOCOL_VERSION,
                    build_id: local_build_id,
                    compatible,
                    platform: Some(local_platform),
                    message: if compatible {
                        None
                    } else {
                        Some(format!(
                            "Protocol version mismatch: agent expects {}, client offered {}",
                            PROTOCOL_VERSION, protocol_version
                        ))
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
                        self.active_locks.push(lock);
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

                let mut cmd = remote_command(&command, &wt, &envs);
                cmd.args(&args);
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
                envs,
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
                let mut cmd = remote_command(&bin, &wt, &envs);
                cmd.arg("cquery")
                    .arg("--output=files")
                    .args(&targets)
                    .args(&flags);

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

                // The Bazel output root is queried at most once, and only if a file lies outside the worktree.
                let mut output_root: Option<PathBuf> = None;
                let mut query_output_root = || -> Result<PathBuf> {
                    if let Some(root) = &output_root {
                        return Ok(root.clone());
                    }
                    let root = query_bazel_output_root(&bin, &wt, &flags, &envs)?;
                    output_root = Some(root.clone());
                    Ok(root)
                };

                for line in stdout_str.lines() {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    let (rel_path, actual_path) =
                        match resolve_artifact_file(&wt, trimmed, &mut query_output_root) {
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

                    if let Err(e) = send_artifact_frames(writer, &rel_path, &actual_path) {
                        let resp = Message::CollectArtifactsResponse {
                            ok: false,
                            artifact_count,
                            error: Some(format!("Failed to send artifact '{}': {}", rel_path, e)),
                        };
                        write_message(writer, &resp)?;
                        return Ok(true);
                    }
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

            Message::CollectPathsRequest { paths } => {
                let wt = match &self.current_worktree {
                    Some(w) => w.clone(),
                    None => {
                        write_message(
                            writer,
                            &Message::CollectPathsResponse {
                                ok: false,
                                artifact_count: 0,
                                error: Some("Worktree lock has not been acquired".to_string()),
                            },
                        )?;
                        return Ok(true);
                    }
                };
                // Report glob/validation failures to the client; a `?` here would end the agent loop.
                let paths = match expand_artifact_globs(&wt, &paths) {
                    Ok(paths) => paths,
                    Err(e) => {
                        write_message(
                            writer,
                            &Message::CollectPathsResponse {
                                ok: false,
                                artifact_count: 0,
                                error: Some(format!("Invalid artifact pattern: {e}")),
                            },
                        )?;
                        return Ok(true);
                    }
                };
                let mut count = 0usize;
                for rel in paths {
                    if let Err(e) = is_safe_relative_path(&rel) {
                        write_message(
                            writer,
                            &Message::CollectPathsResponse {
                                ok: false,
                                artifact_count: count,
                                error: Some(format!("Unsafe artifact path '{}': {}", rel, e)),
                            },
                        )?;
                        return Ok(true);
                    }
                    let path = wt.join(&rel);
                    let canonical_root = fs::canonicalize(&wt)?;
                    let canonical_path = match fs::canonicalize(&path) {
                        Ok(p) if p.starts_with(&canonical_root) => p,
                        Ok(_) => {
                            write_message(
                                writer,
                                &Message::CollectPathsResponse {
                                    ok: false,
                                    artifact_count: count,
                                    error: Some(format!(
                                        "Artifact path '{}' escapes worktree",
                                        rel
                                    )),
                                },
                            )?;
                            return Ok(true);
                        }
                        Err(e) => {
                            write_message(
                                writer,
                                &Message::CollectPathsResponse {
                                    ok: false,
                                    artifact_count: count,
                                    error: Some(format!(
                                        "Artifact '{}' is unavailable: {}",
                                        rel, e
                                    )),
                                },
                            )?;
                            return Ok(true);
                        }
                    };
                    if let Err(e) = send_artifact_frames(writer, &rel, &canonical_path) {
                        write_message(
                            writer,
                            &Message::CollectPathsResponse {
                                ok: false,
                                artifact_count: count,
                                error: Some(e.to_string()),
                            },
                        )?;
                        return Ok(true);
                    }
                    count += 1;
                }
                write_message(
                    writer,
                    &Message::CollectPathsResponse {
                        ok: true,
                        artifact_count: count,
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

/// Builds a remote command in `cwd`. The reserved PATH-prefix env is applied to the inherited PATH
/// and is not exported under its own name.
fn remote_command(program: &str, cwd: &Path, envs: &[(String, String)]) -> Command {
    let mut cmd = Command::new(program);
    cmd.current_dir(cwd);
    for (k, v) in envs {
        if k != PATH_PREPEND_ENV {
            cmd.env(k, v);
        }
    }
    if let Some((_, extra)) = envs.iter().find(|(k, _)| k == PATH_PREPEND_ENV) {
        let inherited = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", join_path(extra, &inherited));
    }
    cmd
}

fn join_path(prepend: &str, inherited: &str) -> String {
    if inherited.is_empty() {
        prepend.to_string()
    } else {
        format!("{prepend}:{inherited}")
    }
}

/// Asks Bazel for its `output_path` in the worktree. No local fallback is attempted.
fn query_bazel_output_root(
    bin: &str,
    worktree: &Path,
    flags: &[String],
    envs: &[(String, String)],
) -> Result<PathBuf> {
    let override_flags: Vec<&str> = flags
        .iter()
        .map(String::as_str)
        .filter(|f| f.starts_with("--override_module="))
        .collect();

    // Repo-local build rc entries can refer to repositories only materialized under a
    // particular build configuration. `info` needs no project rc, and must still work
    // to validate cquery's symlinked output paths.
    let mut cmd1 = remote_command(bin, worktree, envs);
    cmd1.args(["--ignore_all_rc_files", "info"]);
    cmd1.args(&override_flags);
    cmd1.arg("output_path");
    let output = cmd1
        .output()
        .with_context(|| format!("Running '{bin} info output_path'"))?;
    let output = if output.status.success() {
        output
    } else {
        // Keep compatibility with wrappers and older Bazel versions that do not accept
        // --ignore_all_rc_files; their normal info path may still work.
        let mut cmd2 = remote_command(bin, worktree, envs);
        cmd2.arg("info");
        cmd2.args(&override_flags);
        cmd2.arg("output_path");
        cmd2.output()
            .with_context(|| format!("Retrying '{bin} info output_path'"))?
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        anyhow::bail!(
            "'{bin} info output_path' exited with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let root = PathBuf::from(stdout.trim());
    if !root.is_absolute() {
        anyhow::bail!(
            "'{bin} info output_path' did not return an absolute path: '{}'",
            stdout.trim()
        );
    }
    Ok(root)
}

/// Maps a cquery output line to a bazel-bin-relative path and the file to send.
///
/// The file must resolve inside the canonical worktree, or inside the canonical Bazel output root
/// that `output_root` supplies (queried only when the file is outside the worktree).
fn resolve_artifact_file(
    worktree: &Path,
    raw_path_str: &str,
    output_root: &mut dyn FnMut() -> Result<PathBuf>,
) -> Result<(String, PathBuf)> {
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
    // The canonical artifact must lie under the canonical worktree or under the canonical Bazel output root.
    let canonical_worktree = fs::canonicalize(worktree)
        .with_context(|| format!("Canonicalizing worktree '{}'", worktree.display()))?;
    let canonical_actual = fs::canonicalize(&actual_path).with_context(|| {
        format!(
            "Resolving canonical path for artifact '{}'",
            actual_path.display()
        )
    })?;

    if canonical_actual.starts_with(&canonical_worktree) {
        return Ok((rel_under_bin, actual_path));
    }

    let root = output_root().with_context(|| {
        format!(
            "Artifact path '{}' escapes worktree '{}' and the Bazel output root is unavailable",
            raw_path_str,
            worktree.display()
        )
    })?;
    let canonical_root = fs::canonicalize(&root)
        .with_context(|| format!("Canonicalizing Bazel output root '{}'", root.display()))?;
    if !canonical_actual.starts_with(&canonical_root) {
        anyhow::bail!(
            "Artifact path '{}' escapes worktree '{}' and Bazel output root '{}' (resolved to '{}')",
            raw_path_str,
            worktree.display(),
            canonical_root.display(),
            canonical_actual.display()
        );
    }

    Ok((rel_under_bin, actual_path))
}

fn send_artifact_frames<W: Write>(
    writer: &mut W,
    rel_path: &str,
    actual_path: &Path,
) -> Result<()> {
    let meta = fs::symlink_metadata(actual_path)?;
    if meta.file_type().is_symlink() {
        write_message(
            writer,
            &Message::ArtifactChunk(create_artifact_chunk(rel_path, actual_path)?),
        )?;
        return Ok(());
    }
    const CHUNK_BYTES: usize = 1024 * 1024;
    let mut hasher = blake3::Hasher::new();
    let mut file = fs::File::open(actual_path)?;
    let mut buffer = vec![0; CHUNK_BYTES];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    write_message(
        writer,
        &Message::ArtifactStart {
            path: rel_path.to_string(),
            mode: meta.permissions().mode(),
            raw_len: meta.len(),
            blake3: hasher.finalize().to_hex().to_string(),
        },
    )?;
    let mut file = fs::File::open(actual_path)?;
    let mut offset = 0u64;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        write_message(
            writer,
            &Message::ArtifactData {
                path: rel_path.to_string(),
                offset,
                raw_len: n as u64,
                zstd_data: zstd::encode_all(&buffer[..n], 3)?,
            },
        )?;
        offset += n as u64;
    }
    write_message(
        writer,
        &Message::ArtifactEnd {
            path: rel_path.to_string(),
        },
    )?;
    Ok(())
}

fn expand_artifact_globs(root: &Path, patterns: &[String]) -> Result<Vec<String>> {
    if patterns
        .iter()
        .all(|p| !p.contains('*') && !p.contains('?'))
    {
        for pattern in patterns {
            is_safe_relative_path(pattern)?;
        }
        return Ok(patterns
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect());
    }
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    let mut selected = std::collections::BTreeSet::new();
    for pattern in patterns {
        is_safe_relative_path(pattern)?;
        for path in &files {
            if glob_match(pattern.as_bytes(), path.as_bytes()) {
                selected.insert(path.clone());
            }
        }
    }
    Ok(selected.into_iter().collect())
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let mut entries = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_files(root, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            if rel != ".git" && !rel.starts_with(".git/") && !rel.starts_with(".rbuild") {
                out.push(rel);
            }
        }
    }
    Ok(())
}

fn glob_match(pattern: &[u8], value: &[u8]) -> bool {
    let mut dp = vec![vec![false; value.len() + 1]; pattern.len() + 1];
    dp[0][0] = true;
    let mut i = 0;
    while i < pattern.len() {
        if pattern[i] == b'*' {
            let double = i + 1 < pattern.len() && pattern[i + 1] == b'*';
            let next = i + if double { 2 } else { 1 };
            for j in 0..=value.len() {
                if !dp[i][j] {
                    continue;
                }
                dp[next][j] = true;
                let mut k = j;
                while k < value.len() && (double || value[k] != b'/') {
                    k += 1;
                    dp[next][k] = true;
                }
            }
            i = next;
            continue;
        }
        for j in 0..=value.len() {
            if !dp[i][j] {
                continue;
            }
            match pattern[i] {
                b'?' if j < value.len() && value[j] != b'/' => dp[i + 1][j + 1] = true,
                c if j < value.len() && c == value[j] => dp[i + 1][j + 1] = true,
                _ => {}
            }
        }
        i += 1;
    }
    dp[pattern.len()][value.len()]
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

    fn no_output_root() -> Result<PathBuf> {
        anyhow::bail!("no Bazel output root in this test")
    }

    #[test]
    fn test_resolve_artifact_file_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path();

        // 1. bazel-bin layout
        let bin_dir = wt.join("bazel-bin").join("pkg");
        fs::create_dir_all(&bin_dir).unwrap();
        let artifact = bin_dir.join("app");
        fs::write(&artifact, b"binary").unwrap();

        let (rel, path) =
            resolve_artifact_file(wt, "bazel-bin/pkg/app", &mut no_output_root).unwrap();
        assert_eq!(rel, "pkg/app");
        assert_eq!(path, artifact);

        // 2. bazel-out layout
        let out_dir = wt.join("bazel-out/darwin_arm64-fastbuild/bin/pkg");
        fs::create_dir_all(&out_dir).unwrap();
        let artifact2 = out_dir.join("tool");
        fs::write(&artifact2, b"tool_data").unwrap();

        let (rel2, path2) = resolve_artifact_file(
            wt,
            "bazel-out/darwin_arm64-fastbuild/bin/pkg/tool",
            &mut no_output_root,
        )
        .unwrap();
        assert_eq!(rel2, "pkg/tool");
        assert_eq!(path2, artifact2);
    }

    #[test]
    fn test_resolve_artifact_file_accepts_symlinked_bazel_output_root() {
        let wt_dir = tempfile::tempdir().unwrap();
        let wt = wt_dir.path();
        let output_root_dir = tempfile::tempdir().unwrap();
        let output_root = output_root_dir.path();
        let built = output_root.join("k8-fastbuild/bin/pkg");
        fs::create_dir_all(&built).unwrap();
        fs::write(built.join("app"), b"binary").unwrap();
        std::os::unix::fs::symlink(output_root, wt.join("bazel-out")).unwrap();

        let mut root = || -> Result<PathBuf> { Ok(output_root.to_path_buf()) };
        let (rel, path) =
            resolve_artifact_file(wt, "bazel-out/k8-fastbuild/bin/pkg/app", &mut root).unwrap();
        assert_eq!(rel, "pkg/app");
        assert_eq!(path, wt.join("bazel-out/k8-fastbuild/bin/pkg/app"));
    }

    #[test]
    fn test_resolve_artifact_file_rejects_symlink_to_unrelated_root() {
        let wt_dir = tempfile::tempdir().unwrap();
        let wt = wt_dir.path();
        let unrelated_dir = tempfile::tempdir().unwrap();
        let unrelated = unrelated_dir.path();
        fs::create_dir_all(unrelated.join("k8-fastbuild/bin/pkg")).unwrap();
        fs::write(unrelated.join("k8-fastbuild/bin/pkg/app"), b"binary").unwrap();
        std::os::unix::fs::symlink(unrelated, wt.join("bazel-out")).unwrap();

        let real_output_dir = tempfile::tempdir().unwrap();
        let real_output = real_output_dir.path().to_path_buf();
        let mut root = || -> Result<PathBuf> { Ok(real_output.clone()) };
        let res = resolve_artifact_file(wt, "bazel-out/k8-fastbuild/bin/pkg/app", &mut root);
        let err_msg = res.unwrap_err().to_string();
        assert!(
            err_msg.contains("escapes worktree"),
            "Error message must indicate escape: {}",
            err_msg
        );
    }

    #[test]
    fn test_resolve_artifact_file_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path();
        let res = resolve_artifact_file(wt, "bazel-bin/../../../etc/passwd", &mut no_output_root);
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

        let res = resolve_artifact_file(wt, "bazel-bin/pkg/leak_link", &mut no_output_root);
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
    fn generic_glob_matches_nested_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("target/release/deps")).unwrap();
        fs::write(dir.path().join("target/release/rbuild"), b"bin").unwrap();
        fs::write(dir.path().join("target/release/deps/libx.rlib"), b"lib").unwrap();
        let paths = expand_artifact_globs(dir.path(), &["target/release/**".to_string()]).unwrap();
        assert!(paths.contains(&"target/release/rbuild".to_string()));
        assert!(paths.contains(&"target/release/deps/libx.rlib".to_string()));
    }

    #[test]
    fn test_handshake_cross_platform_compatibility() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = AgentSession::new(dir.path().to_path_buf(), None);
        let mut output = Vec::new();

        // Request with matching protocol version but totally different build ID (cross-platform scenario)
        let req = Message::HandshakeRequest {
            protocol_version: PROTOCOL_VERSION,
            build_id: "macos-arm64-different-build-hash".to_string(),
            platform: Some("macos-aarch64".to_string()),
        };

        session.handle_message(&mut output, req).unwrap();
        let mut cursor = std::io::Cursor::new(output);
        let resp: Message = crate::protocol::read_message(&mut cursor).unwrap().unwrap();

        match resp {
            Message::HandshakeResponse {
                compatible,
                protocol_version,
                platform,
                ..
            } => {
                assert!(
                    compatible,
                    "Protocol must be compatible despite different build IDs"
                );
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                assert!(
                    platform.is_some(),
                    "Agent platform identity must be reported"
                );
            }
            other => panic!("Expected HandshakeResponse, got {:?}", other),
        }
    }

    #[test]
    fn test_handshake_rejects_incompatible_protocol_version() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = AgentSession::new(dir.path().to_path_buf(), None);
        let mut output = Vec::new();

        let req = Message::HandshakeRequest {
            protocol_version: PROTOCOL_VERSION + 999,
            build_id: "any-build-id".to_string(),
            platform: Some("linux-x86_64".to_string()),
        };

        session.handle_message(&mut output, req).unwrap();
        let mut cursor = std::io::Cursor::new(output);
        let resp: Message = crate::protocol::read_message(&mut cursor).unwrap().unwrap();

        match resp {
            Message::HandshakeResponse { compatible, .. } => {
                assert!(
                    !compatible,
                    "Incompatible protocol version must be rejected"
                );
            }
            other => panic!("Expected HandshakeResponse, got {:?}", other),
        }
    }
}
