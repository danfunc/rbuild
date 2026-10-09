# rbuild

`rbuild` syncs a Git worktree to a remote agent, runs the selected build tool there, and retrieves selected outputs. The client does not run Bazel or Cargo builds locally.

## Transport and configuration

The local test transport launches an agent child process. SSH transport launches the system `ssh` command with `-T -o BatchMode=yes`; it uses the normal OpenSSH configuration and carries protocol frames on stdin/stdout. The agent's stderr is passed through. SSH has been exercised against the configured main-lxc worker.

Configure `~/.config/rbuild.toml`:

```toml
transport = "ssh"
remote = "user@host-or-ip"
agent_path = "/home/user/.local/bin/rbuild"
build_root = "/srv/build"
client_id = "my-client"
backend = "cargo" # optional: bazel, cargo, generic
bazelisk_path = "bazelisk" # optional: remote name or absolute path
cargo_path = "/home/user/.cargo/bin/cargo" # optional: remote name or absolute path
remote_path_entries = ["/home/user/.local/bin"] # optional: prepended to the remote PATH
excludes = ["target/", "build-temp/"]
artifact_globs = ["out/**/*.bin"] # generic backend; supports *, **, and ?
```

For multiple workers, use named `[[workers]]` entries. The selected/default worker is tried
first; if its SSH connection probe fails, rbuild tries remaining entries in configuration
order. A build or agent protocol error does not trigger failover. Worker Cargo/Bazelisk paths
and PATH entries are independent:

```toml
default_worker = "main-lxc"

[[workers]]
name = "main-lxc"
transport = "ssh"
remote = "ishigakiyua@100.64.0.12"
agent_path = "/home/ishigakiyua/.local/bin/rbuild"
build_root = "/srv/build"
backend = "bazel"
bazelisk_path = "/home/ishigakiyua/.local/bin/bazelisk"
remote_path_entries = ["/home/ishigakiyua/.local/bin"]

[[workers]]
name = "debian-cargo"
transport = "ssh"
remote = "builder@example.net"
agent_path = "/home/builder/.local/bin/rbuild"
backend = "cargo"
cargo_path = "/home/builder/.cargo/bin/cargo"
remote_path_entries = ["/home/builder/.cargo/bin"]
```

Select with `rbuild --worker debian-cargo build ...`, or inspect with
`rbuild workers --json`. When a cross-platform agent reports an old protocol, rbuild archives
its own Rust sources, builds the agent with the worker's native `cargo`, and atomically replaces
the configured agent path. This uses the worker's platform/toolchain and avoids deploying a
host-platform executable. It requires Cargo and its dependencies to be available on the worker.

CLI options such as `--transport`, `--remote`, `--agent-path`, `--backend`, `--build-root`, and `--client-id` override settings. `--bazelisk-path`, `--cargo-path`, and `--remote-path-entry` (repeatable) set the remote tools and PATH; a `--remote-path-entry` replaces the configured `remote_path_entries`. `--artifacts final|all` selects Cargo outputs (see below). For example:

```sh
rbuild --transport=ssh --remote=user@100.64.0.12 --agent-path=/home/user/.local/bin/rbuild build //pkg:app
```

Do not use an SSH alias whose address is stale. SSH does not upload the local client executable. On a cross-platform protocol mismatch, rbuild builds its own source on the worker and atomically replaces `agent_path`.

## Backend selection

An explicit `backend` setting wins. Otherwise `MODULE.bazel`, `WORKSPACE`, or root `BUILD` selects Bazel; `Cargo.toml` selects Cargo; other repositories use the generic command backend.

- **Bazel** runs `bazelisk <args>` remotely and retrieves `build` outputs using `bazelisk cquery --output=files`. A cquery path is accepted when it resolves inside the canonical worktree, or inside the canonical `output_path` reported by `bazelisk info output_path`. That root is queried only when a file resolves outside the worktree, for example through a `bazel-out` symlink. Paths under `bazel-out/<config>/bin/` map to `bazel-bin`-relative paths such as `pkg/app`.
- **Cargo** runs `cargo build --message-format=json`. The JSON is parsed by rbuild; only the rendered compiler messages and a `build-finished` summary are printed. Raw JSON is not printed, and stderr and the exit code pass through. By default the retrieved files are executables and `cdylib`/`staticlib`/`dylib` outputs. Build script outputs, `.rlib`/`.rmeta`/`.d` files, `proc-macro` outputs, and dSYM bundles are skipped. `proc-macro` is excluded because it is a compile-time input for dependent crates, not a deliverable. `--artifacts all` retrieves every file cargo reports, including those.
- **Generic** executes the first argument as a remote program and remaining arguments as its argv. Configured `artifact_globs` select output files relative to the remote worktree.

There is no local build fallback. If the remote bazelisk or cargo is missing, the command fails.

Remote PATH: `remote_path_entries` (or `--remote-path-entry`) are prepended to the PATH inherited by the agent, for build, cquery, and generic commands. rbuild does not add `$HOME/.local/bin` or `$HOME/.cargo/bin` by default. The client's `$HOME` is not the remote user's home directory, so the client cannot name those paths correctly, and the agent's PATH depends on the remote login environment. Set `cargo_path` or `remote_path_entries` explicitly when a tool lives in one of those directories.

Build artifacts are placed under `.rbuild-out/`. `compile_commands.json` there has remote-worktree paths rewritten to the local repository root. `rbuild-debug-map.json` contains GDB `set substitute-path` and VS Code cppdbg `sourceFileMap` values.

Unknown Bazel flags whose values cannot be safely separated from target labels cause artifact collection to be skipped with a warning. The build command itself still receives its original arguments.

## Protocol and filesystem behavior

Frames use a 4-byte big-endian length and a 64 MiB maximum serialized payload. Artifact files are sent as 1 MiB raw chunks, zstd-compressed independently, then checked for offset, length, and BLAKE3 before atomic placement. Workspace sync files still use one frame per file and therefore must fit the frame limit.

Workspace enumeration uses `git ls-files -co --exclude-standard`; non-Git source directories are not supported. It preserves executable bits and symlink entries, rejects path traversal and escaping symlinks, and uses BLAKE3 metadata caching plus whole-file zstd compression. Deleted files are removed from the remote worktree.

Wire compatibility: `PROTOCOL_VERSION` (currently 2) must match exactly on both sides. The agent answers `compatible=false` to any other version, and the client also refuses a `compatible=true` answer that reports a different version. Protocol 2 added `CollectArtifactsRequest.envs` and the reserved `RBUILD_PATH_PREPEND` env key, so an agent on protocol 1 would silently ignore PATH entries. The handshake therefore fails closed in every mismatch case, and no lock or build request is sent.

Mismatch outcomes:
- Different protocol, different OS/architecture: the worker-native source build updates the agent.
- Different protocol, same OS/architecture, local binary available (local transport): the client sends its binary and the agent replaces its own file. The session then fails with "reconnect and retry", because the running agent process still has the old code. The next connection starts the new binary.
- The worker-native build requires Cargo, a Rust toolchain, and dependency access or cache on the worker.
- Same protocol, different build ID: accepted. Build IDs differ across macOS and Linux as expected, and do not cause an upload.

SSH sessions never send the local executable to the remote agent.

Worktree locks use an atomic `.rbuild.lock` file containing a PID. On Linux, missing `/proc/<pid>` entries are treated as stale; elsewhere `kill -0` checks liveness. PID reuse can keep a stale lock from being recovered. Old worktree retention/cleanup is not implemented.

## Status and verification limits

Implemented: local and SSH transport code, protocol/platform handshake, Git differential sync, Bazel/Cargo/generic backend selection, chunked artifact retrieval, compile database rewriting, debugger mapping, stale-PID lock recovery.

Not yet verified against a live remote host: Cargo build/retrieval on Debian, second-run differential behavior over SSH, and remote failure exit-code forwarding. Workspace sync chunking, non-Git traversal, and worktree garbage collection remain future work.

Local checks: `cargo fmt --check`, `cargo check --all-targets`, and `cargo test`.
