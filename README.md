# rbuild

`rbuild` syncs a Git worktree to a remote agent, runs the selected build tool there, and retrieves selected outputs. The client does not run Bazel or Cargo builds locally.

## Transport and configuration

The local test transport launches an agent child process. SSH transport launches the system `ssh` command with `-T -o BatchMode=yes`; it uses the normal OpenSSH configuration and carries protocol frames on stdin/stdout. The agent's stderr is passed through. SSH transport has not yet been validated against a live host.

Configure `~/.config/rbuild.toml`:

```toml
transport = "ssh"
remote = "user@host-or-ip"
agent_path = "/home/user/.local/bin/rbuild"
build_root = "/srv/build"
client_id = "my-client"
backend = "cargo" # optional: bazel, cargo, generic
excludes = ["target/", "build-temp/"]
artifact_globs = ["out/**/*.bin"] # generic backend; supports *, **, and ?
```

CLI options such as `--transport`, `--remote`, `--agent-path`, `--backend`, `--build-root`, and `--client-id` override settings. For example:

```sh
rbuild --transport=ssh --remote=user@100.64.0.12 --agent-path=/home/user/.local/bin/rbuild build //pkg:app
```

Do not use an SSH alias whose address is stale. The remote agent binary must already exist at `agent_path`; SSH mode does not upload the local client executable. Native-build it on the target or provide a matching platform asset in a later release workflow.

## Backend selection

An explicit `backend` setting wins. Otherwise `MODULE.bazel`, `WORKSPACE`, or root `BUILD` selects Bazel; `Cargo.toml` selects Cargo; other repositories use the generic command backend.

- **Bazel** runs `bazelisk <args>` remotely and retrieves `build` outputs using `bazelisk cquery --output=files`.
- **Cargo** runs `cargo build --message-format=json`, parses `compiler-artifact` messages, and retrieves reported worktree-relative files.
- **Generic** executes the first argument as a remote program and remaining arguments as its argv. Configured `artifact_globs` select output files relative to the remote worktree.

Build artifacts are placed under `.rbuild-out/`. `compile_commands.json` there has remote-worktree paths rewritten to the local repository root. `rbuild-debug-map.json` contains GDB `set substitute-path` and VS Code cppdbg `sourceFileMap` values.

Unknown Bazel flags whose values cannot be safely separated from target labels cause artifact collection to be skipped with a warning. The build command itself still receives its original arguments.

## Protocol and filesystem behavior

Frames use a 4-byte big-endian length and a 64 MiB maximum serialized payload. Artifact files are sent as 1 MiB raw chunks, zstd-compressed independently, then checked for offset, length, and BLAKE3 before atomic placement. Workspace sync files still use one frame per file and therefore must fit the frame limit.

Workspace enumeration uses `git ls-files -co --exclude-standard`; non-Git source directories are not supported. It preserves executable bits and symlink entries, rejects path traversal and escaping symlinks, and uses BLAKE3 metadata caching plus whole-file zstd compression. Deleted files are removed from the remote worktree.

The handshake compares protocol versions. OS/architecture and binary build IDs are reported separately; a build ID difference is expected across macOS and Linux and does not cause a client binary upload. SSH sessions never self-update the remote agent from the local executable.

Worktree locks use an atomic `.rbuild.lock` file containing a PID. On Linux, missing `/proc/<pid>` entries are treated as stale; elsewhere `kill -0` checks liveness. PID reuse can keep a stale lock from being recovered. Old worktree retention/cleanup is not implemented.

## Status and verification limits

Implemented: local and SSH transport code, protocol/platform handshake, Git differential sync, Bazel/Cargo/generic backend selection, chunked artifact retrieval, compile database rewriting, debugger mapping, stale-PID lock recovery.

Not yet verified against a live remote host: SSH login/agent startup, native Linux agent deployment, Cargo build/retrieval on Debian, Bazel build/retrieval, second-run differential behavior over SSH, and remote failure exit-code forwarding. Workspace sync chunking, non-Git traversal, worktree garbage collection, and cross-compiled Linux agent packaging remain future work.

Local checks: `cargo fmt --check`, `cargo check --all-targets`, and `cargo test`.
