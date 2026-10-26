use rbuild::agent::run_agent_loop;
use rbuild::agent::updater::update_agent_binary;
use rbuild::client::ClientSession;
use rbuild::client::transport::{LocalProcessTransport, StreamTransport, Transport};
use rbuild::protocol::message::{Message, SyncFileChunk};
use rbuild::sync::manifest::ManifestCache;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::thread;

/// Helper to spin up an agent thread connected via UnixStream socket pair.
fn setup_agent_client_pair(
    base_dir: &Path,
) -> (
    ClientSession<StreamTransport<UnixStream, UnixStream>>,
    thread::JoinHandle<()>,
) {
    let (client_sock, agent_sock) = UnixStream::pair().expect("UnixStream pair");
    let agent_sock_read = agent_sock.try_clone().expect("clone agent sock");
    let base = base_dir.to_path_buf();

    let agent_handle = thread::spawn(move || {
        let _ = run_agent_loop(agent_sock_read, agent_sock, base, None);
    });

    let client_sock_read = client_sock.try_clone().expect("clone client sock");
    let transport = StreamTransport::new(client_sock_read, client_sock);
    let session = ClientSession::new(transport);

    (session, agent_handle)
}

/// Helper to initialize a real git repository with configured user identity.
fn init_git_repo(path: &Path) {
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap_or_else(|e| panic!("Failed running git {:?}: {}", args, e));
        assert!(status.success(), "git command {:?} failed", args);
    };

    run(&["init"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test User"]);
    run(&["config", "core.excludesfile", ""]);
}

#[test]
fn test_end_to_end_sync_and_differential_update_with_git() {
    let temp_root = tempfile::tempdir().unwrap();
    let source_dir = temp_root.path().join("source");
    let remote_base = temp_root.path().join("srv_build");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&remote_base).unwrap();

    // 1. Initialize real Git repo
    init_git_repo(&source_dir);

    // Create .gitignore to verify ignore handling
    let gitignore = source_dir.join(".gitignore");
    fs::write(&gitignore, b"ignored.txt\nignored_dir/\n").unwrap();

    // Tracked committed file
    let file1 = source_dir.join("file1.txt");
    fs::write(&file1, b"version 1 of file 1").unwrap();

    // Executable script
    let script = source_dir.join("script.sh");
    fs::write(&script, b"#!/bin/sh\necho hello\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    // Symlink
    let symlink = source_dir.join("link_to_file1.txt");
    std::os::unix::fs::symlink("file1.txt", &symlink).unwrap();

    // Commit tracked files
    let run_git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&source_dir)
            .status()
            .unwrap();
        assert!(status.success());
    };
    run_git(&[
        "add",
        "-f",
        ".gitignore",
        "file1.txt",
        "script.sh",
        "link_to_file1.txt",
    ]);
    run_git(&["commit", "-m", "initial commit"]);

    // Create untracked non-ignored file
    let nested_dir = source_dir.join("sub/dir");
    fs::create_dir_all(&nested_dir).unwrap();
    let untracked_file = nested_dir.join("untracked.txt");
    fs::write(&untracked_file, b"untracked content").unwrap();

    // Create ignored files that should NOT be transferred
    let ignored_file = source_dir.join("ignored.txt");
    fs::write(&ignored_file, b"should be ignored").unwrap();
    let ignored_dir = source_dir.join("ignored_dir");
    fs::create_dir_all(&ignored_dir).unwrap();
    fs::write(ignored_dir.join("sub_ignored.txt"), b"also ignored").unwrap();

    // Start agent and client
    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();

    let assigned_path = session.lock_worktree("user1", "myrepo", "feature").unwrap();
    let remote_wt = Path::new(&assigned_path);
    assert!(remote_wt.exists(), "Remote worktree directory must exist");

    // First sync: must include tracked (.gitignore, file1, script, link) AND untracked (sub/dir/untracked.txt)
    let mut cache = ManifestCache::default();
    let stats1 = session
        .sync_workspace(&source_dir, &mut cache, &[])
        .unwrap();

    // Total 5 files: .gitignore, file1.txt, script.sh, link_to_file1.txt, sub/dir/untracked.txt
    assert_eq!(stats1.files_checked, 5);
    assert_eq!(stats1.files_transferred, 5);
    assert_eq!(stats1.files_deleted, 0);

    // Verify remote files
    assert_eq!(
        fs::read(remote_wt.join("file1.txt")).unwrap(),
        b"version 1 of file 1"
    );
    let remote_script_meta = fs::metadata(remote_wt.join("script.sh")).unwrap();
    assert_eq!(
        remote_script_meta.permissions().mode() & 0o777,
        0o755,
        "Executable permission must be preserved"
    );
    assert_eq!(
        fs::read(remote_wt.join("sub/dir/untracked.txt")).unwrap(),
        b"untracked content",
        "Untracked non-ignored file must be synced"
    );
    let link_target = fs::read_link(remote_wt.join("link_to_file1.txt")).unwrap();
    assert_eq!(link_target, Path::new("file1.txt"));

    // Verify ignored files are NOT synced
    assert!(
        !remote_wt.join("ignored.txt").exists(),
        "Ignored file must not be synced"
    );
    assert!(
        !remote_wt.join("ignored_dir").exists(),
        "Ignored directory must not be synced"
    );

    // 2. Second sync: modified file1, delete untracked.txt, add new_file.txt
    fs::write(&file1, b"version 2 of file 1 (modified)").unwrap();
    fs::remove_file(&untracked_file).unwrap();
    let new_file = source_dir.join("new_file.txt");
    fs::write(&new_file, b"brand new file").unwrap();

    let stats2 = session
        .sync_workspace(&source_dir, &mut cache, &[])
        .unwrap();

    // 5 files checked (.gitignore, file1, script, link, new_file)
    assert_eq!(stats2.files_checked, 5);
    // 2 transferred (file1 modified, new_file added)
    assert_eq!(stats2.files_transferred, 2);
    // 1 deleted (sub/dir/untracked.txt)
    assert_eq!(stats2.files_deleted, 1);

    // Verify updates and deletions on agent
    assert_eq!(
        fs::read(remote_wt.join("file1.txt")).unwrap(),
        b"version 2 of file 1 (modified)"
    );
    assert_eq!(
        fs::read(remote_wt.join("new_file.txt")).unwrap(),
        b"brand new file"
    );
    assert!(
        !remote_wt.join("sub/dir/untracked.txt").exists(),
        "Deleted file must be removed on remote"
    );

    // Verify remote manifest cache was persisted and pruned deleted files
    let remote_cache_path = remote_wt.join(".rbuild/manifest_cache.json");
    assert!(
        remote_cache_path.exists(),
        "Remote worktree manifest cache must be persisted"
    );
    let remote_cache = ManifestCache::load_from_file(&remote_cache_path);
    assert!(remote_cache.entries.contains_key("file1.txt"));
    assert!(remote_cache.entries.contains_key("new_file.txt"));
    assert!(!remote_cache.entries.contains_key("sub/dir/untracked.txt"));

    // 3. Third sync with no changes must transfer 0 files and delete 0 files
    let stats3 = session
        .sync_workspace(&source_dir, &mut cache, &[])
        .unwrap();
    assert_eq!(stats3.files_checked, 5);
    assert_eq!(stats3.files_transferred, 0);
    assert_eq!(stats3.files_deleted, 0);

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_pipe_deadlock_concurrent_large_output() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    fs::create_dir_all(&remote_base).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    let wt = session.lock_worktree("user1", "repo", "main").unwrap();

    // Create a script that generates > 128 KiB to both stdout and stderr simultaneously,
    // which would cause pipe buffer deadlock if drained sequentially.
    let script_path = Path::new(&wt).join("flood.sh");
    let script_content = r#"#!/bin/sh
dd if=/dev/zero bs=1024 count=128 2>/dev/null | tr '\0' 'A'
dd if=/dev/zero bs=1024 count=128 2>/dev/null | tr '\0' 'B' >&2
exit 42
"#;
    fs::write(&script_path, script_content).unwrap();
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

    let mut captured_stdout = Vec::new();
    let mut captured_stderr = Vec::new();
    let exit_code = session
        .run_remote_command_to_writers(
            &script_path.to_string_lossy(),
            &[],
            &[],
            &mut captured_stdout,
            &mut captured_stderr,
        )
        .expect("Command execution should complete without pipe deadlock");

    assert_eq!(
        exit_code, 42,
        "Must capture exact nonzero child process exit code"
    );
    assert_eq!(
        captured_stdout.len(),
        128 * 1024,
        "All 128 KiB of stdout must be drained without truncation"
    );
    assert_eq!(
        captured_stderr.len(),
        128 * 1024,
        "All 128 KiB of stderr must be drained without truncation"
    );

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_artifact_collection_with_cquery_mapping() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    let client_out_dir = temp_root.path().join(".rbuild-out");
    fs::create_dir_all(&remote_base).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    let wt_str = session.lock_worktree("user1", "myrepo", "main").unwrap();
    let wt = Path::new(&wt_str);

    // Create fake bazel-bin layout on agent
    let bin_dir = wt.join("bazel-bin/pkg");
    fs::create_dir_all(&bin_dir).unwrap();
    let app_binary = bin_dir.join("my_app");
    fs::write(&app_binary, b"compiled application binary").unwrap();
    fs::set_permissions(&app_binary, fs::Permissions::from_mode(0o755)).unwrap();

    let data_file = bin_dir.join("data.txt");
    fs::write(&data_file, b"metadata info").unwrap();

    // Create a fake bazelisk executable inside the worktree
    let fake_bazelisk = wt.join("fake_bazelisk.sh");
    let script_content = r#"#!/bin/sh
if [ "$1" = "cquery" ]; then
    echo "bazel-out/darwin_arm64-fastbuild/bin/pkg/my_app"
    echo "bazel-bin/pkg/data.txt"
    exit 0
fi
exit 1
"#;
    fs::write(&fake_bazelisk, script_content).unwrap();
    fs::set_permissions(&fake_bazelisk, fs::Permissions::from_mode(0o755)).unwrap();

    // Request artifact collection using the fake bazelisk command
    let count = session
        .collect_artifacts(
            &client_out_dir,
            Some(&fake_bazelisk.to_string_lossy()),
            &["//pkg:my_app".to_string()],
            &[],
        )
        .expect("Artifact collection should succeed");

    assert_eq!(count, 2, "Expected 2 artifacts collected");

    // Verify files placed in .rbuild-out with bazel-bin relative layout
    let local_app = client_out_dir.join("pkg/my_app");
    assert!(local_app.exists(), "Collected application must exist");
    assert_eq!(
        fs::read(&local_app).unwrap(),
        b"compiled application binary"
    );
    let app_meta = fs::metadata(&local_app).unwrap();
    assert_eq!(
        app_meta.permissions().mode() & 0o777,
        0o755,
        "Artifact executable permission must be preserved"
    );

    let local_data = client_out_dir.join("pkg/data.txt");
    assert!(local_data.exists(), "Collected data file must exist");
    assert_eq!(fs::read(&local_data).unwrap(), b"metadata info");

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_artifact_collection_path_traversal_rejected() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    let client_out_dir = temp_root.path().join(".rbuild-out");
    fs::create_dir_all(&remote_base).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    let wt_str = session.lock_worktree("user1", "myrepo", "main").unwrap();
    let wt = Path::new(&wt_str);

    // Create fake bazelisk emitting malicious path traversal
    let fake_bazelisk = wt.join("evil_bazelisk.sh");
    let script_content = r#"#!/bin/sh
if [ "$1" = "cquery" ]; then
    echo "bazel-bin/../../outside_secret.txt"
    exit 0
fi
exit 1
"#;
    fs::write(&fake_bazelisk, script_content).unwrap();
    fs::set_permissions(&fake_bazelisk, fs::Permissions::from_mode(0o755)).unwrap();

    let res = session.collect_artifacts(
        &client_out_dir,
        Some(&fake_bazelisk.to_string_lossy()),
        &["//evil:target".to_string()],
        &[],
    );

    assert!(
        res.is_err(),
        "Malicious path traversal in cquery output must be rejected"
    );

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_artifact_collection_symlink_escape_rejected() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    let client_out_dir = temp_root.path().join(".rbuild-out");
    fs::create_dir_all(&remote_base).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    let wt_str = session.lock_worktree("user1", "myrepo", "main").unwrap();
    let wt = Path::new(&wt_str);

    // Create an external secret file outside the worktree
    let outside_dir = tempfile::tempdir().unwrap();
    let secret_file = outside_dir.path().join("secret.key");
    fs::write(&secret_file, b"super_secret_credentials").unwrap();

    // Create symlink inside worktree pointing outside
    let bin_dir = wt.join("bazel-bin/pkg");
    fs::create_dir_all(&bin_dir).unwrap();
    let link_target = bin_dir.join("leaked_artifact");
    std::os::unix::fs::symlink(&secret_file, &link_target).unwrap();

    // Fake bazelisk returning the symlink artifact
    let fake_bazelisk = wt.join("leak_bazelisk.sh");
    let script_content = r#"#!/bin/sh
if [ "$1" = "cquery" ]; then
    echo "bazel-bin/pkg/leaked_artifact"
    exit 0
fi
exit 1
"#;
    fs::write(&fake_bazelisk, script_content).unwrap();
    fs::set_permissions(&fake_bazelisk, fs::Permissions::from_mode(0o755)).unwrap();

    let res = session.collect_artifacts(
        &client_out_dir,
        Some(&fake_bazelisk.to_string_lossy()),
        &["//pkg:leaked_artifact".to_string()],
        &[],
    );

    assert!(
        res.is_err(),
        "Symlink escaping worktree in cquery output must be rejected"
    );
    let err_msg = res.unwrap_err().to_string();
    assert!(
        err_msg.contains("escapes worktree"),
        "Error message must specify worktree escape: {}",
        err_msg
    );
    assert!(
        !client_out_dir.join("pkg/leaked_artifact").exists(),
        "No artifact should be written to client output directory on escape rejection"
    );

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_artifact_collection_large_file_is_chunked_and_reassembled() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    let client_out_dir = temp_root.path().join(".rbuild-out");
    fs::create_dir_all(&remote_base).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    let wt_str = session.lock_worktree("user1", "myrepo", "main").unwrap();
    let wt = Path::new(&wt_str);

    // Create a file larger than the former whole-frame artifact limit.
    let bin_dir = wt.join("bazel-bin/pkg");
    fs::create_dir_all(&bin_dir).unwrap();
    let huge_file_path = bin_dir.join("huge_output");
    let f = fs::File::create(&huge_file_path).unwrap();
    let large_len = 20 * 1024 * 1024 + 123;
    f.set_len(large_len).unwrap();

    // Fake bazelisk returning the oversized artifact
    let fake_bazelisk = wt.join("huge_bazelisk.sh");
    let script_content = r#"#!/bin/sh
if [ "$1" = "cquery" ]; then
    echo "bazel-bin/pkg/huge_output"
    exit 0
fi
exit 1
"#;
    fs::write(&fake_bazelisk, script_content).unwrap();
    fs::set_permissions(&fake_bazelisk, fs::Permissions::from_mode(0o755)).unwrap();

    let res = session.collect_artifacts(
        &client_out_dir,
        Some(&fake_bazelisk.to_string_lossy()),
        &["//pkg:huge_output".to_string()],
        &[],
    );

    assert_eq!(res.unwrap(), 1, "Large artifact must be collected");
    let local_file = client_out_dir.join("pkg/huge_output");
    assert_eq!(fs::metadata(&local_file).unwrap().len(), large_len);
    assert_eq!(
        blake3::hash(&fs::read(local_file).unwrap()),
        blake3::hash(&vec![0; large_len as usize])
    );

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_collect_paths_large_file_is_chunked_and_reassembled() {
    // Cargo and generic backends use collect_paths, so this covers their >16 MiB path.
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    let client_out_dir = temp_root.path().join(".rbuild-out");
    fs::create_dir_all(&remote_base).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    let wt_str = session.lock_worktree("user1", "myrepo", "main").unwrap();
    let wt = Path::new(&wt_str);

    let release_dir = wt.join("target/release");
    fs::create_dir_all(&release_dir).unwrap();
    let large_len = 20 * 1024 * 1024 + 123;
    let f = fs::File::create(release_dir.join("app")).unwrap();
    f.set_len(large_len).unwrap();

    let count = session
        .collect_paths(&client_out_dir, &["target/release/*".to_string()])
        .expect("glob-selected large artifact must be collected");
    assert_eq!(count, 1);
    let local_file = client_out_dir.join("target/release/app");
    assert_eq!(fs::metadata(&local_file).unwrap().len(), large_len);
    assert_eq!(
        blake3::hash(&fs::read(local_file).unwrap()),
        blake3::hash(&vec![0; large_len as usize])
    );

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_collect_paths_invalid_pattern_is_reported_not_disconnected() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    let client_out_dir = temp_root.path().join(".rbuild-out");
    fs::create_dir_all(&remote_base).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    session.lock_worktree("user1", "myrepo", "main").unwrap();

    // Before the fix the agent loop ended on this error, so the client saw a bare EOF.
    let err = session
        .collect_paths(&client_out_dir, &["../*".to_string()])
        .err()
        .expect("traversal pattern must be rejected");
    assert!(
        err.to_string().contains("Invalid artifact pattern"),
        "unexpected error: {err}"
    );

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_artifact_collection_query_failure_reported() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    let client_out_dir = temp_root.path().join(".rbuild-out");
    fs::create_dir_all(&remote_base).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    let wt_str = session.lock_worktree("user1", "myrepo", "main").unwrap();
    let wt = Path::new(&wt_str);

    // Create fake bazelisk that exits with error on cquery
    let fake_bazelisk = wt.join("failing_bazelisk.sh");
    let script_content = r#"#!/bin/sh
echo "ERROR: Target //broken:target failed analysis" >&2
exit 1
"#;
    fs::write(&fake_bazelisk, script_content).unwrap();
    fs::set_permissions(&fake_bazelisk, fs::Permissions::from_mode(0o755)).unwrap();

    let res = session.collect_artifacts(
        &client_out_dir,
        Some(&fake_bazelisk.to_string_lossy()),
        &["//broken:target".to_string()],
        &[],
    );

    assert!(
        res.is_err(),
        "Failed cquery execution must report error and fail collection"
    );
    let err_msg = res.unwrap_err().to_string();
    assert!(
        err_msg.contains("Target //broken:target failed analysis")
            || err_msg.contains("cquery exited")
    );

    drop(session);
    let _ = agent_thread.join();
}

/// Writes a fake bazelisk that answers `info output_path` and `cquery`.
/// When `require_path_prefix` is set, `cquery` fails unless PATH starts with that directory.
fn write_fake_bazelisk(
    dir: &Path,
    output_path: &Path,
    cquery_line: &str,
    require_path_prefix: Option<&Path>,
) -> std::path::PathBuf {
    let require = match require_path_prefix {
        Some(prefix) => format!("{}:*", prefix.display()),
        None => "*".to_string(),
    };
    let log_file = dir.join("bazel_calls.log");
    let script = format!(
        r#"#!/bin/sh
echo "CMD: $@" >> "{log}"
if [ "$1" = "--ignore_all_rc_files" ] && [ "$2" = "info" ]; then
    echo "{output}"
    exit 0
fi
if [ "$1" = "info" ]; then
    echo "{output}"
    exit 0
fi
if [ "$1" = "cquery" ]; then
    case "$PATH" in
        {require}) ;;
        *) echo "PATH lacks the configured prefix" >&2; exit 3 ;;
    esac
    echo "{line}"
    exit 0
fi
exit 1
"#,
        log = log_file.display(),
        output = output_path.display(),
        require = require,
        line = cquery_line,
    );
    let fake = dir.join("fake_bazelisk.sh");
    fs::write(&fake, script).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    fake
}

#[test]
fn test_artifact_collection_accepts_symlinked_bazel_output_root() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    let client_out_dir = temp_root.path().join(".rbuild-out");
    let output_root = temp_root.path().join("bazel_output_root");
    let marker_bin = temp_root.path().join("marker_bin");
    fs::create_dir_all(&remote_base).unwrap();
    fs::create_dir_all(output_root.join("k8-fastbuild/bin/pkg")).unwrap();
    fs::write(output_root.join("k8-fastbuild/bin/pkg/app"), b"built app").unwrap();
    fs::create_dir_all(&marker_bin).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    let wt_str = session.lock_worktree("user1", "myrepo", "main").unwrap();
    let wt = Path::new(&wt_str);
    // Bazel-created layout: worktree/bazel-out is a symlink to the real output root outside the worktree.
    std::os::unix::fs::symlink(&output_root, wt.join("bazel-out")).unwrap();

    let fake = write_fake_bazelisk(
        temp_root.path(),
        &output_root,
        "bazel-out/k8-fastbuild/bin/pkg/app",
        Some(&marker_bin),
    );
    let envs = rbuild::protocol::message::path_prepend_envs(&[marker_bin.display().to_string()]);

    let count = session
        .collect_artifacts_with_envs(
            &client_out_dir,
            Some(&fake.to_string_lossy()),
            &["//pkg:app".to_string()],
            &[],
            &envs,
        )
        .expect("Artifact under the reported Bazel output root must be accepted");

    assert_eq!(count, 1);
    assert_eq!(
        fs::read(client_out_dir.join("pkg/app")).unwrap(),
        b"built app"
    );

    // Verify info output_path arguments when flags are empty (traditional arguments with no extra flags)
    let log_file = temp_root.path().join("bazel_calls.log");
    let log_content = fs::read_to_string(&log_file).unwrap();
    let info_line = log_content
        .lines()
        .find(|line| line.contains("info") && line.contains("output_path"))
        .expect("info output_path must be called");
    assert_eq!(
        info_line, "CMD: --ignore_all_rc_files info output_path",
        "When flags are empty, info output_path args must be identical to traditional call"
    );

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_artifact_collection_rejects_symlink_to_unrelated_root() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    let client_out_dir = temp_root.path().join(".rbuild-out");
    let unrelated = temp_root.path().join("unrelated_root");
    let real_output_root = temp_root.path().join("bazel_output_root");
    fs::create_dir_all(&remote_base).unwrap();
    fs::create_dir_all(unrelated.join("k8-fastbuild/bin/pkg")).unwrap();
    fs::write(unrelated.join("k8-fastbuild/bin/pkg/app"), b"foreign app").unwrap();
    fs::create_dir_all(&real_output_root).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    let wt_str = session.lock_worktree("user1", "myrepo", "main").unwrap();
    let wt = Path::new(&wt_str);
    std::os::unix::fs::symlink(&unrelated, wt.join("bazel-out")).unwrap();

    let fake = write_fake_bazelisk(
        temp_root.path(),
        &real_output_root,
        "bazel-out/k8-fastbuild/bin/pkg/app",
        None,
    );

    let err = session
        .collect_artifacts(
            &client_out_dir,
            Some(&fake.to_string_lossy()),
            &["//pkg:app".to_string()],
            &[],
        )
        .expect_err("Artifact under an unrelated root must be rejected");
    assert!(
        format!("{err:#}").contains("escapes worktree"),
        "Error must report the escape: {err:#}"
    );
    assert!(!client_out_dir.join("pkg/app").exists());

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_artifact_collection_output_root_query_filters_flags() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    let client_out_dir = temp_root.path().join(".rbuild-out");
    let output_root = temp_root.path().join("bazel_output_root");
    fs::create_dir_all(&remote_base).unwrap();
    fs::create_dir_all(output_root.join("k8-fastbuild/bin/pkg")).unwrap();
    fs::write(output_root.join("k8-fastbuild/bin/pkg/app"), b"built app").unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    let wt_str = session.lock_worktree("user1", "myrepo", "main").unwrap();
    let wt = Path::new(&wt_str);
    std::os::unix::fs::symlink(&output_root, wt.join("bazel-out")).unwrap();

    let fake = write_fake_bazelisk(
        temp_root.path(),
        &output_root,
        "bazel-out/k8-fastbuild/bin/pkg/app",
        None,
    );

    let flags = vec![
        "--config=opt".to_string(),
        "--override_module=dep_a=/remote/dep_a".to_string(),
        "--copt=-O2".to_string(),
        "--override_module=dep_b=/remote/dep_b".to_string(),
    ];

    let count = session
        .collect_artifacts_with_envs(
            &client_out_dir,
            Some(&fake.to_string_lossy()),
            &["//pkg:app".to_string()],
            &flags,
            &[],
        )
        .expect("Artifact under the reported Bazel output root must be accepted");

    assert_eq!(count, 1);
    let log_file = temp_root.path().join("bazel_calls.log");
    let log_content = fs::read_to_string(&log_file).unwrap();
    let info_line = log_content
        .lines()
        .find(|line| line.contains("info") && line.contains("output_path"))
        .expect("info output_path must be called");
    assert_eq!(
        info_line,
        "CMD: --ignore_all_rc_files info --override_module=dep_a=/remote/dep_a --override_module=dep_b=/remote/dep_b output_path",
        "Only --override_module flags must be passed to info output_path, directly after info"
    );

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_remote_path_prepend_applies_to_commands_without_exporting_key() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    fs::create_dir_all(&remote_base).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    session.lock_worktree("user1", "myrepo", "main").unwrap();

    let envs = rbuild::protocol::message::path_prepend_envs(&[
        "/opt/rbuild-test/bin".to_string(),
        " ".to_string(),
        "/usr/local/rbuild-test/bin".to_string(),
    ]);
    let script = "printf '%s' \"$PATH\"; printf ' %s' \"${RBUILD_PATH_PREPEND-unset}\"";
    let (code, out, _) = session
        .run_remote_command_capture("sh", &["-c".to_string(), script.to_string()], &envs)
        .unwrap();

    assert_eq!(code, 0);
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.starts_with("/opt/rbuild-test/bin:/usr/local/rbuild-test/bin:"),
        "Configured entries must lead PATH: {text}"
    );
    assert!(
        text.ends_with(" unset"),
        "Reserved key must not be exported to the command: {text}"
    );

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_agent_binary_self_update_in_isolated_temp_dir() {
    let temp_root = tempfile::tempdir().unwrap();
    let isolated_agent_dir = temp_root.path().join("agent_bin");
    fs::create_dir_all(&isolated_agent_dir).unwrap();
    let target_agent_bin = isolated_agent_dir.join("rbuild_agent");

    // Write initial dummy agent binary
    fs::write(&target_agent_bin, b"original agent binary v1").unwrap();
    fs::set_permissions(&target_agent_bin, fs::Permissions::from_mode(0o755)).unwrap();

    // Prepare update payload
    let new_binary_data = b"new agent binary v2 executable";
    let raw_len = new_binary_data.len() as u64;
    let expected_hash = blake3::hash(new_binary_data).to_hex().to_string();
    let zstd_data = zstd::encode_all(&new_binary_data[..], 3).unwrap();

    // Test 1: Successful atomic update with valid hash
    update_agent_binary(&target_agent_bin, &zstd_data, raw_len, &expected_hash)
        .expect("Agent binary update should succeed");

    assert_eq!(
        fs::read(&target_agent_bin).unwrap(),
        new_binary_data,
        "Agent binary must be atomically replaced with new content"
    );
    let meta = fs::metadata(&target_agent_bin).unwrap();
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o755,
        "Executable permission must be preserved"
    );

    // Test 2: Checksum mismatch rejected
    let bad_hash = "0000000000000000000000000000000000000000000000000000000000000000";
    let update_fail = update_agent_binary(&target_agent_bin, &zstd_data, raw_len, bad_hash);
    assert!(
        update_fail.is_err(),
        "Must reject update with mismatched BLAKE3 hash"
    );
    // Content remains v2
    assert_eq!(fs::read(&target_agent_bin).unwrap(), new_binary_data);
}

#[test]
fn test_cli_local_process_integration() {
    // Build or locate rbuild binary
    let temp_root = tempfile::tempdir().unwrap();
    let repo_dir = temp_root.path().join("repo");
    let remote_base = temp_root.path().join("remote_base");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::create_dir_all(&remote_base).unwrap();

    init_git_repo(&repo_dir);
    fs::write(repo_dir.join("main.rs"), b"fn main() {}").unwrap();

    let run_git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&repo_dir)
            .status()
            .unwrap();
        assert!(status.success());
    };
    run_git(&["add", "main.rs"]);
    run_git(&["commit", "-m", "init"]);

    // Locate compiled rbuild binary
    let mut exe = std::env::current_exe().unwrap();
    exe.pop(); // pop test binary name
    if exe.file_name().map(|n| n == "deps").unwrap_or(false) {
        exe.pop(); // pop deps/
    }
    let target_bin = exe.join("rbuild");
    if !target_bin.exists() {
        let status = Command::new("cargo")
            .args(["build", "--bin", "rbuild"])
            .status()
            .expect("cargo build rbuild");
        assert!(status.success());
    }

    // Spawn local process transport
    let transport = LocalProcessTransport::spawn(&target_bin, Some(&remote_base))
        .expect("Spawn local process transport");
    let mut session = ClientSession::new(transport);

    session.handshake(Some(&target_bin)).expect("Handshake");

    let wt = session
        .lock_worktree("test_client", "test_repo", "main")
        .expect("Lock worktree");

    let mut cache = ManifestCache::default();
    let stats = session
        .sync_workspace(&repo_dir, &mut cache, &[])
        .expect("Sync workspace");
    assert_eq!(stats.files_checked, 1);
    assert_eq!(stats.files_transferred, 1);
    assert!(Path::new(&wt).join("main.rs").exists());

    // Execute remote command
    let exit_code = session
        .run_remote_command("echo", &["integration test passed".to_string()], &[])
        .expect("Run echo command");
    assert_eq!(exit_code, 0);
}

#[test]
fn test_path_traversal_attack_rejected() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    fs::create_dir_all(&remote_base).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    let _ = session.lock_worktree("user1", "myrepo", "main").unwrap();

    // Attempt to inject a file chunk with path traversal
    let malicious_chunk = SyncFileChunk {
        path: "../../../evil.txt".to_string(),
        mode: 0o644,
        is_symlink: false,
        symlink_target: None,
        raw_len: 4,
        blake3: blake3::hash(b"evil").to_hex().to_string(),
        zstd_data: zstd::encode_all(&b"evil"[..], 3).unwrap(),
    };

    session
        .transport_mut()
        .send(&Message::SyncFileChunk(malicious_chunk))
        .unwrap();

    match session.transport_mut().recv().unwrap() {
        Message::SyncFileResponse { ok, error, .. } => {
            assert!(!ok, "Agent must reject traversal file chunks");
            assert!(
                error.is_some(),
                "Agent should return an error message for traversal"
            );
        }
        other => panic!("Expected SyncFileResponse, got {:?}", other),
    }

    assert!(!temp_root.path().join("evil.txt").exists());

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_symlink_escape_rejected() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    fs::create_dir_all(&remote_base).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    let _ = session.lock_worktree("user1", "myrepo", "main").unwrap();

    let malicious_symlink = SyncFileChunk {
        path: "pwn_link.txt".to_string(),
        mode: 0o777,
        is_symlink: true,
        symlink_target: Some("/etc/passwd".to_string()),
        raw_len: 0,
        blake3: blake3::hash(b"/etc/passwd").to_hex().to_string(),
        zstd_data: Vec::new(),
    };

    session
        .transport_mut()
        .send(&Message::SyncFileChunk(malicious_symlink))
        .unwrap();

    match session.transport_mut().recv().unwrap() {
        Message::SyncFileResponse { ok, error, .. } => {
            assert!(!ok, "Agent must reject escaping symlinks");
            assert!(error.is_some());
        }
        other => panic!("Expected SyncFileResponse, got {:?}", other),
    }

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_data_corruption_and_hash_verification() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    fs::create_dir_all(&remote_base).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    let _ = session.lock_worktree("user1", "myrepo", "main").unwrap();

    let payload = b"actual uncorrupted payload";
    let corrupted_chunk = SyncFileChunk {
        path: "corrupt.txt".to_string(),
        mode: 0o644,
        is_symlink: false,
        symlink_target: None,
        raw_len: payload.len() as u64,
        blake3: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        zstd_data: zstd::encode_all(&payload[..], 3).unwrap(),
    };

    session
        .transport_mut()
        .send(&Message::SyncFileChunk(corrupted_chunk))
        .unwrap();

    match session.transport_mut().recv().unwrap() {
        Message::SyncFileResponse { ok, error, .. } => {
            assert!(!ok, "Agent must reject corrupted payload");
            assert!(error.unwrap().contains("BLAKE3 checksum mismatch"));
        }
        other => panic!("Expected SyncFileResponse, got {:?}", other),
    }

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_worktree_concurrent_lock_rejection() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    fs::create_dir_all(&remote_base).unwrap();

    let (mut session1, agent_thread1) = setup_agent_client_pair(&remote_base);
    session1.handshake(None).unwrap();
    session1
        .lock_worktree("client_a", "repo_a", "main")
        .unwrap();

    // Second session attempting to lock the exact same worktree
    let (mut session2, agent_thread2) = setup_agent_client_pair(&remote_base);
    session2.handshake(None).unwrap();
    let lock_attempt = session2.lock_worktree("client_a", "repo_a", "main");
    assert!(
        lock_attempt.is_err(),
        "Second lock on identical worktree must fail"
    );

    drop(session1);
    let _ = agent_thread1.join();

    // Now session2 should be able to acquire lock
    let lock_retry = session2.lock_worktree("client_a", "repo_a", "main");
    assert!(
        lock_retry.is_ok(),
        "Lock should be acquireable after first session disconnects"
    );

    drop(session2);
    let _ = agent_thread2.join();
}

#[test]
fn test_remote_command_execution() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    fs::create_dir_all(&remote_base).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    let wt = session.lock_worktree("user1", "myrepo", "main").unwrap();

    fs::write(Path::new(&wt).join("marker.txt"), b"echo test").unwrap();

    let exit_code = session
        .run_remote_command("echo", &["remote execution test".to_string()], &[])
        .unwrap();
    assert_eq!(exit_code, 0, "Remote command must exit with code 0");

    drop(session);
    let _ = agent_thread.join();
}

#[test]
fn test_cli_cargo_backend_collects_compiler_artifact() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let remote = temp.path().join("remote");
    let fake_bin = temp.path().join("bin");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&remote).unwrap();
    fs::create_dir_all(&fake_bin).unwrap();
    init_git_repo(&repo);
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname='demo'\nversion='0.1.0'\nedition='2024'\n",
    )
    .unwrap();
    fs::write(repo.join("src.rs"), "fn main() {}\n").unwrap();
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["add", "Cargo.toml", "src.rs"]);
    git(&["commit", "-m", "init"]);

    let cargo_build = Command::new("cargo")
        .args(["build", "--bin", "rbuild"])
        .status()
        .unwrap();
    assert!(cargo_build.success());

    let fake_cargo = fake_bin.join("cargo");
    fs::write(&fake_cargo, r##"#!/bin/sh
mkdir -p target/debug
printf 'fake-linux-binary' > target/debug/demo
echo "{\"reason\":\"compiler-artifact\",\"filenames\":[\"$(pwd)/target/debug/demo\"],\"executable\":\"$(pwd)/target/debug/demo\"}"
"##).unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let mut exe = std::env::current_exe().unwrap();
    exe.pop();
    if exe.file_name().is_some_and(|n| n == "deps") {
        exe.pop();
    }
    let rbuild = exe.join("rbuild");
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Command::new(rbuild)
        .args(["--transport=local", "--backend", "cargo", "--build-root"])
        .arg(&remote)
        .args(["build", "--release"])
        .current_dir(&repo)
        .env("PATH", path)
        .env("RBUILD_CONFIG", temp.path().join("no-user-config.toml"))
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "Cargo backend must finish successfully: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let artifact = repo.join(".rbuild-out/target/debug/demo");
    let bytes = fs::read(artifact).unwrap_or_else(|e| {
        panic!(
            "artifact missing: {e}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(bytes, b"fake-linux-binary");
    assert!(repo.join(".rbuild-out/rbuild-debug-map.json").exists());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("compiler-artifact"),
        "Raw cargo JSON must not reach stdout: {stdout}"
    );
    assert!(stdout.contains("rbuild: Cargo reported 1 artifact path(s)."));
}

#[test]
fn test_cli_client_binary_end_to_end_build_and_artifacts() {
    let temp_root = tempfile::tempdir().unwrap();
    let repo_dir = temp_root.path().join("repo");
    let remote_base = temp_root.path().join("remote_base");
    let fake_bin_dir = temp_root.path().join("fake_bin");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::create_dir_all(&remote_base).unwrap();
    fs::create_dir_all(&fake_bin_dir).unwrap();

    // 1. Initialize git repo on client
    init_git_repo(&repo_dir);
    let build_file = repo_dir.join("BUILD");
    fs::write(&build_file, b"# dummy build file").unwrap();
    let src_file = repo_dir.join("app.c");
    fs::write(&src_file, b"int main() { return 0; }").unwrap();

    let run_git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&repo_dir)
            .status()
            .unwrap();
        assert!(status.success());
    };
    run_git(&["add", "BUILD", "app.c"]);
    run_git(&["commit", "-m", "init commit"]);

    // 2. Create fake bazelisk on PATH
    let fake_bazelisk = fake_bin_dir.join("bazelisk");
    let fake_script = r#"#!/bin/sh
if [ "$1" = "build" ]; then
    echo "FAKE_BAZELISK_STDOUT: Building //pkg:myapp"
    echo "FAKE_BAZELISK_STDERR: Compiling app.c" >&2
    mkdir -p bazel-bin/pkg
    echo "compiled_binary_bytes_98765" > bazel-bin/pkg/myapp
    chmod 755 bazel-bin/pkg/myapp
    exit 0
elif [ "$1" = "cquery" ]; then
    echo "bazel-bin/pkg/myapp"
    exit 0
fi
echo "Unknown command $1" >&2
exit 1
"#;
    fs::write(&fake_bazelisk, fake_script).unwrap();
    fs::set_permissions(&fake_bazelisk, fs::Permissions::from_mode(0o755)).unwrap();

    // Locate rbuild binary
    let mut exe = std::env::current_exe().unwrap();
    exe.pop();
    if exe.file_name().map(|n| n == "deps").unwrap_or(false) {
        exe.pop();
    }
    let rbuild_bin = exe.join("rbuild");
    if !rbuild_bin.exists() {
        let status = Command::new("cargo")
            .args(["build", "--bin", "rbuild"])
            .status()
            .expect("cargo build rbuild");
        assert!(status.success());
    }

    let current_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), current_path);

    // 3. Execute rbuild client executable
    let output = Command::new(&rbuild_bin)
        .arg("--transport=local")
        .arg("--build-root")
        .arg(&remote_base)
        .arg("build")
        .arg("//pkg:myapp")
        .current_dir(&repo_dir)
        .env("PATH", &new_path)
        .env(
            "RBUILD_CONFIG",
            temp_root.path().join("no-user-config.toml"),
        )
        .output()
        .expect("Failed to execute rbuild client");

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let stderr_str = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(0),
        "rbuild client must exit with code 0. Stderr: {}",
        stderr_str
    );

    // Verify stdout/stderr streaming from remote command
    assert!(
        stdout_str.contains("FAKE_BAZELISK_STDOUT: Building //pkg:myapp"),
        "Stdout must contain streamed output from remote bazel. Got: {}",
        stdout_str
    );
    assert!(
        stderr_str.contains("FAKE_BAZELISK_STDERR: Compiling app.c"),
        "Stderr must contain streamed error output from remote bazel. Got: {}",
        stderr_str
    );

    // Verify artifacts collected into local .rbuild-out
    let collected_app = repo_dir.join(".rbuild-out/pkg/myapp");
    assert!(
        collected_app.exists(),
        "Artifact must be downloaded into .rbuild-out"
    );
    assert_eq!(
        fs::read(&collected_app).unwrap(),
        b"compiled_binary_bytes_98765\n"
    );
    let meta = fs::metadata(&collected_app).unwrap();
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o755,
        "Collected artifact must preserve executable permissions"
    );

    // Verify local workspace was NOT used for bazel build directly
    assert!(
        !repo_dir.join("bazel-bin").exists(),
        "Local repo must not have bazel-bin (build executed remotely)"
    );
}

#[test]
fn test_cli_client_binary_nonzero_exit_status() {
    let temp_root = tempfile::tempdir().unwrap();
    let repo_dir = temp_root.path().join("repo");
    let remote_base = temp_root.path().join("remote_base");
    let fake_bin_dir = temp_root.path().join("fake_bin");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::create_dir_all(&remote_base).unwrap();
    fs::create_dir_all(&fake_bin_dir).unwrap();

    init_git_repo(&repo_dir);
    fs::write(repo_dir.join("BUILD"), "# fake Bazel workspace marker\n").unwrap();
    fs::write(repo_dir.join("main.c"), b"int main() {}").unwrap();

    let run_git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&repo_dir)
            .status()
            .unwrap();
        assert!(status.success());
    };
    run_git(&["add", "BUILD", "main.c"]);
    run_git(&["commit", "-m", "init"]);

    let fake_bazelisk = fake_bin_dir.join("bazelisk");
    let fake_script = r#"#!/bin/sh
echo "Simulating compile error" >&2
exit 37
"#;
    fs::write(&fake_bazelisk, fake_script).unwrap();
    fs::set_permissions(&fake_bazelisk, fs::Permissions::from_mode(0o755)).unwrap();

    let mut exe = std::env::current_exe().unwrap();
    exe.pop();
    if exe.file_name().map(|n| n == "deps").unwrap_or(false) {
        exe.pop();
    }
    let rbuild_bin = exe.join("rbuild");
    if !rbuild_bin.exists() {
        let status = Command::new("cargo")
            .args(["build", "--bin", "rbuild"])
            .status()
            .expect("cargo build rbuild");
        assert!(status.success());
    }

    let current_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), current_path);

    let output = Command::new(&rbuild_bin)
        .arg("--transport=local")
        .arg("--build-root")
        .arg(&remote_base)
        .arg("build")
        .arg("//pkg:failing")
        .current_dir(&repo_dir)
        .env("PATH", &new_path)
        .env(
            "RBUILD_CONFIG",
            temp_root.path().join("no-user-config.toml"),
        )
        .output()
        .expect("Failed to execute rbuild client");

    assert_eq!(
        output.status.code(),
        Some(37),
        "rbuild client must exit with the exact nonzero exit code (37) from bazel"
    );
    let stderr_str = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr_str.contains("Simulating compile error"),
        "Client stderr must stream error output from failed command"
    );

    assert!(
        !repo_dir.join(".rbuild-out").exists(),
        "Artifacts must not be collected on build failure"
    );
}

#[test]
fn test_cli_progress_json_reports_sync_build_and_collect() {
    let temp_root = tempfile::tempdir().unwrap();
    let repo_dir = temp_root.path().join("repo");
    let remote_base = temp_root.path().join("remote_base");
    let fake_bin_dir = temp_root.path().join("fake_bin");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::create_dir_all(&remote_base).unwrap();
    fs::create_dir_all(&fake_bin_dir).unwrap();
    init_git_repo(&repo_dir);
    fs::write(repo_dir.join("BUILD"), b"# dummy").unwrap();
    fs::write(repo_dir.join("app.c"), b"int main() { return 0; }").unwrap();
    let fake_bazelisk = fake_bin_dir.join("bazelisk");
    fs::write(
        &fake_bazelisk,
        "#!/bin/sh\nif [ \"$1\" = build ]; then mkdir -p bazel-bin/pkg; echo bytes > bazel-bin/pkg/myapp; exit 0; fi\nif [ \"$1\" = cquery ]; then echo bazel-bin/pkg/myapp; exit 0; fi\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&fake_bazelisk, fs::Permissions::from_mode(0o755)).unwrap();

    let mut exe = std::env::current_exe().unwrap();
    exe.pop();
    if exe.file_name().map(|n| n == "deps").unwrap_or(false) {
        exe.pop();
    }
    let rbuild_bin = exe.join("rbuild");
    if !rbuild_bin.exists() {
        let status = Command::new("cargo")
            .args(["build", "--bin", "rbuild"])
            .status()
            .expect("cargo build rbuild");
        assert!(status.success());
    }
    let new_path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Command::new(&rbuild_bin)
        .arg("--progress=json")
        .arg("--transport=local")
        .arg("--build-root")
        .arg(&remote_base)
        .arg("build")
        .arg("//pkg:myapp")
        .current_dir(&repo_dir)
        .env("PATH", &new_path)
        .env(
            "RBUILD_CONFIG",
            temp_root.path().join("no-user-config.toml"),
        )
        .output()
        .expect("Failed to execute rbuild client");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");

    let events: Vec<serde_json::Value> = stderr
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|v: &serde_json::Value| v["event"] == "rbuild.progress")
        .collect();
    let find = |phase: &str, state: &str| {
        events
            .iter()
            .find(|v| v["phase"] == phase && v["state"] == state)
            .unwrap_or_else(|| panic!("no {phase}/{state} event in {events:?}"))
    };
    let sync_done = find("sync", "done");
    assert!(sync_done["total_files"].as_u64().unwrap() >= 2);
    assert_eq!(sync_done["done_files"], sync_done["total_files"]);
    assert_eq!(sync_done["done_bytes"], sync_done["total_bytes"]);
    find("build", "start");
    let collect_done = find("collect", "done");
    assert_eq!(collect_done["done_files"], 1);
    assert_eq!(collect_done["done_bytes"], 6);
}

#[test]
fn test_end_to_end_dependency_sync_and_isolation() {
    let temp_root = tempfile::tempdir().unwrap();
    let main_repo = temp_root.path().join("main_repo");
    let dep_repo = temp_root.path().join("dep_repo");
    let remote_base = temp_root.path().join("srv_build");
    fs::create_dir_all(&main_repo).unwrap();
    fs::create_dir_all(&dep_repo).unwrap();
    fs::create_dir_all(&remote_base).unwrap();

    // 1. Initialize git repos
    init_git_repo(&main_repo);
    init_git_repo(&dep_repo);

    fs::write(main_repo.join("main.c"), b"int main() { return 0; }").unwrap();
    fs::write(main_repo.join("main_local.txt"), b"local untracked").unwrap();

    let run_git = |repo: &Path, args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .unwrap();
        assert!(status.success());
    };
    run_git(&main_repo, &["add", "main.c"]);
    run_git(&main_repo, &["commit", "-m", "init main"]);

    fs::write(dep_repo.join("dep_lib.c"), b"void helper() {}").unwrap();
    fs::write(dep_repo.join("dep_untracked.txt"), b"dep untracked change").unwrap();
    run_git(&dep_repo, &["add", "dep_lib.c"]);
    run_git(&dep_repo, &["commit", "-m", "init dep"]);

    // 2. Setup agent session
    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();

    // 3. Lock and sync dependency first
    let dep_wt = session
        .lock_worktree("client1", "dep_repo", "branch_a")
        .expect("Lock dep worktree");
    let mut dep_cache = ManifestCache::default();
    let dep_stats = session
        .sync_workspace(&dep_repo, &mut dep_cache, &[])
        .expect("Sync dep workspace");
    // Committed dep_lib.c and untracked dep_untracked.txt are transferred
    assert_eq!(dep_stats.files_transferred, 2);

    // 4. Lock and sync main repository second (must be locked last)
    let main_wt = session
        .lock_worktree("client1", "main_repo", "branch_a")
        .expect("Lock main worktree");
    let mut main_cache = ManifestCache::default();
    let main_stats = session
        .sync_workspace(&main_repo, &mut main_cache, &[])
        .expect("Sync main workspace");
    assert_eq!(main_stats.files_transferred, 2);

    // 5. Verify worker paths match worker_dep_path
    let expected_dep_path = rbuild::config::worker_dep_path(
        remote_base.to_str().unwrap(),
        "client1",
        "dep_repo",
        "branch_a",
    );
    let expected_main_path = rbuild::config::worker_dep_path(
        remote_base.to_str().unwrap(),
        "client1",
        "main_repo",
        "branch_a",
    );
    assert_eq!(Path::new(&dep_wt), expected_dep_path);
    assert_eq!(Path::new(&main_wt), expected_main_path);

    // Verify files on agent
    assert!(Path::new(&dep_wt).join("dep_lib.c").exists());
    assert!(Path::new(&dep_wt).join("dep_untracked.txt").exists());
    assert!(Path::new(&main_wt).join("main.c").exists());
    assert!(Path::new(&main_wt).join("main_local.txt").exists());
    assert!(!Path::new(&main_wt).join("dep_lib.c").exists());

    // 6. Verify current_worktree on agent is main_repo
    let exit_code = session
        .run_remote_command("test", &["-f".to_string(), "main.c".to_string()], &[])
        .expect("Remote test main.c exists in current worktree");
    assert_eq!(exit_code, 0);

    let exit_code_dep = session
        .run_remote_command("test", &["-f".to_string(), "dep_lib.c".to_string()], &[])
        .expect("Remote test dep_lib.c in main worktree");
    assert_ne!(exit_code_dep, 0);

    // 7. Verify accessing dependency via RBUILD_DEP_* environment variable
    let env_name = rbuild::config::dep_env_var_name("dep_repo");
    assert_eq!(env_name, "RBUILD_DEP_DEP_REPO");
    let envs = vec![(env_name, dep_wt.clone())];
    let exit_code_env = session
        .run_remote_command(
            "sh",
            &[
                "-c".to_string(),
                "test -f \"$RBUILD_DEP_DEP_REPO/dep_untracked.txt\"".to_string(),
            ],
            &envs,
        )
        .expect("Remote check file via RBUILD_DEP_DEP_REPO env");
    assert_eq!(exit_code_env, 0);

    drop(session);
    agent_thread.join().unwrap();
}

#[test]
fn test_cli_dependency_workspace_sync_and_bazel_overrides() {
    let temp_root = tempfile::tempdir().unwrap();
    let main_repo = temp_root.path().join("main_project");
    let dep_repo = temp_root.path().join("shizuku_dep");
    let remote_base = temp_root.path().join("remote_base");
    let fake_bin_dir = temp_root.path().join("fake_bin");
    fs::create_dir_all(&main_repo).unwrap();
    fs::create_dir_all(&dep_repo).unwrap();
    fs::create_dir_all(&remote_base).unwrap();
    fs::create_dir_all(&fake_bin_dir).unwrap();

    // 1. Initialize git repos
    init_git_repo(&main_repo);
    init_git_repo(&dep_repo);

    fs::write(main_repo.join("BUILD"), b"# main build file").unwrap();
    fs::write(main_repo.join("app.c"), b"int main() { return 0; }").unwrap();

    let run_git = |repo: &Path, args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .unwrap();
        assert!(status.success());
    };
    run_git(&main_repo, &["add", "BUILD", "app.c"]);
    run_git(&main_repo, &["commit", "-m", "init main"]);

    // Main rbuild.toml specifying dependency
    let rbuild_toml = r#"
[deps.shizuku]
path = "../shizuku_dep"
bazel_module = "shizuku"
"#;
    fs::write(main_repo.join("rbuild.toml"), rbuild_toml).unwrap();

    fs::write(dep_repo.join("BUILD"), b"# dep build file").unwrap();
    fs::write(dep_repo.join("lib.c"), b"void dep() {}").unwrap();
    run_git(&dep_repo, &["add", "BUILD", "lib.c"]);
    run_git(&dep_repo, &["commit", "-m", "init dep"]);
    // Untracked file in dep to verify uncommitted changes are synchronized
    fs::write(
        dep_repo.join("untracked_in_dep.txt"),
        b"uncommitted dep content",
    )
    .unwrap();

    // 2. Setup fake bazelisk logging its arguments and env vars
    let log_file = temp_root.path().join("bazel_calls.log");
    let fake_output_root = temp_root.path().join("fake_output_root");
    fs::create_dir_all(&fake_output_root).unwrap();
    let fake_bazelisk = fake_bin_dir.join("bazelisk");
    let fake_script = format!(
        r#"#!/bin/sh
echo "CMD: $@" >> "{log}"
echo "ENV_SHIZUKU: $RBUILD_DEP_SHIZUKU" >> "{log}"
if [ "$1" = "build" ]; then
    mkdir -p "{output_root}/k8-fastbuild/bin/pkg"
    echo "compiled_output" > "{output_root}/k8-fastbuild/bin/pkg/myapp"
    chmod 755 "{output_root}/k8-fastbuild/bin/pkg/myapp"
    ln -sfn "{output_root}" bazel-out
    exit 0
elif [ "$1" = "cquery" ]; then
    echo "bazel-out/k8-fastbuild/bin/pkg/myapp"
    exit 0
elif [ "$1" = "--ignore_all_rc_files" ] && [ "$2" = "info" ]; then
    echo "{output_root}"
    exit 0
elif [ "$1" = "info" ]; then
    echo "{output_root}"
    exit 0
fi
echo "Unknown $1" >&2
exit 1
"#,
        log = log_file.display(),
        output_root = fake_output_root.display()
    );
    fs::write(&fake_bazelisk, fake_script).unwrap();
    fs::set_permissions(&fake_bazelisk, fs::Permissions::from_mode(0o755)).unwrap();

    // Locate rbuild binary
    let mut exe = std::env::current_exe().unwrap();
    exe.pop();
    if exe.file_name().map(|n| n == "deps").unwrap_or(false) {
        exe.pop();
    }
    let rbuild_bin = exe.join("rbuild");
    if !rbuild_bin.exists() {
        let status = Command::new("cargo")
            .args(["build", "--bin", "rbuild"])
            .status()
            .expect("cargo build rbuild");
        assert!(status.success());
    }

    let new_path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    // 3. Run rbuild client
    let output = Command::new(&rbuild_bin)
        .arg("--transport=local")
        .arg("--build-root")
        .arg(&remote_base)
        .arg("--client-id=test_client")
        .arg("build")
        .arg("//pkg:myapp")
        .current_dir(&main_repo)
        .env("PATH", &new_path)
        .env(
            "RBUILD_CONFIG",
            temp_root.path().join("no-user-config.toml"),
        )
        .output()
        .expect("Failed to execute rbuild client");

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let stderr_str = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "rbuild client failed. Stderr: {}\nStdout: {}",
        stderr_str,
        stdout_str
    );

    // 4. Verify artifact downloaded to main_repo
    assert!(main_repo.join(".rbuild-out/pkg/myapp").exists());

    // 5. Verify dependency worker path and synchronized files
    let expected_dep_dir = remote_base.join("test_client/shizuku/main");
    assert!(expected_dep_dir.join("lib.c").exists());
    assert!(expected_dep_dir.join("untracked_in_dep.txt").exists());

    // Verify main worker path and synchronized files
    let repo_name = main_repo.file_name().unwrap().to_str().unwrap();
    let expected_main_dir = remote_base.join(format!("test_client/{}/main", repo_name));
    assert!(expected_main_dir.join("app.c").exists());

    // 6. Verify dependency manifest cache saved in main_repo/.rbuild/deps/, not in dep_repo
    assert!(!dep_repo.join(".rbuild").exists());
    assert!(
        main_repo
            .join(".rbuild/deps/shizuku_manifest_cache.json")
            .exists()
    );

    // 7. Verify Bazel args log contains --override_module=shizuku=<worker_dep_path>
    let log_content = fs::read_to_string(&log_file).unwrap();
    let expected_override_flag =
        format!("--override_module=shizuku={}", expected_dep_dir.display());
    assert!(
        log_content.contains(&expected_override_flag),
        "Expected override flag '{}' in log:\n{}",
        expected_override_flag,
        log_content
    );

    // Verify cquery command also received the override flag
    let cquery_line = log_content
        .lines()
        .find(|line| line.starts_with("CMD: cquery"))
        .expect("cquery command executed");
    assert!(
        cquery_line.contains(&expected_override_flag),
        "cquery command must receive override flag. Got: {}",
        cquery_line
    );

    // Verify info output_path command also received the override flag right after info
    let info_line = log_content
        .lines()
        .find(|line| line.contains("info") && line.contains("output_path"))
        .expect("info output_path command executed");
    assert!(
        info_line.contains(&expected_override_flag),
        "info output_path command must receive override flag. Got: {}",
        info_line
    );
    let expected_info_seq = format!("info {} output_path", expected_override_flag);
    assert!(
        info_line.contains(&expected_info_seq),
        "info output_path command must place override flag immediately after info. Got: {}",
        info_line
    );

    // 8. Verify environment variable RBUILD_DEP_SHIZUKU was set
    let expected_env_line = format!("ENV_SHIZUKU: {}", expected_dep_dir.display());
    assert!(
        log_content.contains(&expected_env_line),
        "Expected env line '{}' in log:\n{}",
        expected_env_line,
        log_content
    );
}

#[test]
fn test_cquery_multiple_targets_single_expression_and_unquotable_rejection() {
    let temp_root = tempfile::tempdir().unwrap();
    let remote_base = temp_root.path().join("srv_build");
    let client_out_dir = temp_root.path().join(".rbuild-out");
    fs::create_dir_all(&remote_base).unwrap();

    let (mut session, agent_thread) = setup_agent_client_pair(&remote_base);
    session.handshake(None).unwrap();
    let wt_str = session.lock_worktree("user1", "myrepo", "main").unwrap();
    let wt = Path::new(&wt_str);

    // 複数成果物（2つ以上のファイル）を配置
    let bin_dir = wt.join("bazel-bin/pkg");
    fs::create_dir_all(&bin_dir).unwrap();
    let file1 = bin_dir.join("file1.txt");
    fs::write(&file1, b"content of file 1").unwrap();
    let file2 = bin_dir.join("file2.txt");
    fs::write(&file2, b"content of file 2").unwrap();

    // 引数を記録する fake bazelisk スクリプトを作成
    let log_file = wt.join("cquery_argv.log");
    let fake_bazelisk = wt.join("fake_bazelisk.sh");
    let script_content = format!(
        r#"#!/bin/sh
if [ "$1" = "cquery" ]; then
    for arg in "$@"; do
        echo "$arg" >> "{log}"
    done
    echo "bazel-bin/pkg/file1.txt"
    echo "bazel-bin/pkg/file2.txt"
    exit 0
fi
exit 1
"#,
        log = log_file.display()
    );
    fs::write(&fake_bazelisk, script_content).unwrap();
    fs::set_permissions(&fake_bazelisk, fs::Permissions::from_mode(0o755)).unwrap();

    // 1. 複数ターゲットを指定して成果物回収を実行
    let targets = vec!["//pkg:target1".to_string(), "//pkg:target2".to_string()];
    let flags = vec!["--flag_example=1".to_string()];
    let count = session
        .collect_artifacts(
            &client_out_dir,
            Some(&fake_bazelisk.to_string_lossy()),
            &targets,
            &flags,
        )
        .expect("Artifact collection for multiple targets must succeed");

    // 複数成果物（2つ以上のファイル）が実際に回収されることを assert
    assert_eq!(count, 2, "Expected 2 artifacts collected");
    let local_file1 = client_out_dir.join("pkg/file1.txt");
    let local_file2 = client_out_dir.join("pkg/file2.txt");
    assert!(local_file1.exists(), "Collected file1 must exist");
    assert!(local_file2.exists(), "Collected file2 must exist");
    assert_eq!(
        fs::read(&local_file1).unwrap(),
        b"content of file 1",
        "file1 content must match"
    );
    assert_eq!(
        fs::read(&local_file2).unwrap(),
        b"content of file 2",
        "file2 content must match"
    );

    // fake Bazel が cquery 呼び出しで「単一の query 式 argv」を受け取ることを記録/検証
    let log_content = fs::read_to_string(&log_file).unwrap();
    let received_args: Vec<&str> = log_content.lines().collect();
    assert_eq!(
        received_args.len(),
        4,
        "cquery command must have exactly 4 argv elements (cquery, --output=files, query_expr, flags). Got: {:?}",
        received_args
    );
    assert_eq!(received_args[0], "cquery");
    assert_eq!(received_args[1], "--output=files");
    assert_eq!(
        received_args[2], r#"set("//pkg:target1" "//pkg:target2")"#,
        "Multiple targets must be passed as a single set(...) query expression argv"
    );
    assert_eq!(
        received_args[3], "--flag_example=1",
        "Flags must be passed as argv after the query expression"
    );

    // 2. 引用不能ラベルで成果物回収を試行し、ok:false かつ cquery が呼ばれないことを assert
    // ログファイルをクリア
    fs::write(&log_file, b"").unwrap();

    let unquotable_targets = vec![r#"//pkg:bad"quote'mix"#.to_string()];
    let unquotable_res = session.collect_artifacts(
        &client_out_dir,
        Some(&fake_bazelisk.to_string_lossy()),
        &unquotable_targets,
        &[],
    );
    assert!(
        unquotable_res.is_err(),
        "Artifact collection must fail for unquotable targets"
    );

    let log_after_unquotable = fs::read_to_string(&log_file).unwrap();
    assert!(
        log_after_unquotable.is_empty(),
        "cquery must NOT be called when targets are unquotable, but log was: {}",
        log_after_unquotable
    );

    drop(session);
    let _ = agent_thread.join();
}
