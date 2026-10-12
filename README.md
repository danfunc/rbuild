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
remote = "ishigakiyua@main-lxc.private.danfuncl.com"
client_id = "macbook" # optional; overrides the top-level client_id
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

CLI options such as `--transport`, `--remote`, `--agent-path`, `--backend`, `--build-root`, and `--client-id` override settings. Client identity precedence is `--client-id`, selected worker's `client_id`, top-level `client_id`, then the local username. `--bazelisk-path`, `--cargo-path`, and `--remote-path-entry` (repeatable) set the remote tools and PATH; a `--remote-path-entry` replaces the configured `remote_path_entries`. `--artifacts final|all` selects Cargo outputs (see below). For example:

```sh
rbuild --transport=ssh --remote=user@main-lxc.private.danfuncl.com --agent-path=/home/user/.local/bin/rbuild build //pkg:app
```

Do not use an SSH alias whose address is stale. SSH does not upload the local client executable. On a cross-platform protocol mismatch, rbuild builds its own source on the worker and atomically replaces `agent_path`.

## Machine-readable progress

`rbuild --progress=json ...` (the flag must come first) writes one JSON object per line to stderr, throttled to at most one `progress` line per 100 ms (`start`/`done` lines are never dropped): `{"event":"rbuild.progress","phase":"sync|build|collect","state":"start|progress|done", "done_files":..,"total_files":..,"done_bytes":..,"total_bytes":..}` for transfers (`collect` omits totals, which are unknown in advance) and `{"event":"rbuild.job","name":..,"index":..,"total":..}` at the start of each `rbuild job` stage. Human output is unchanged. The VS Code extension consumes these lines.

## Backend selection

Configuration is split by purpose. The repository's `rbuild.toml` supplies
project jobs; `~/.config/rbuild.toml` supplies worker connections and tool
paths. Worker configuration precedence is `--config`, then `RBUILD_CONFIG`,
then `~/.config/rbuild.toml`; the project file is always read from the current
repository. A job's `backend` overrides the selected worker's backend and
automatic backend selection. Its `worker` chooses a worker from the user
configuration; `local` is reserved for commands run directly on the client.

```toml
[jobs.build]
worker = "main-lxc"
backend = "bazel"
args = ["build", "//firmware:shizuku"]
artifacts = ["bazel-bin/firmware/shizuku"]

[jobs.run]
worker = "local"
command = ["/bin/sh", "-c", "picotool load -f \"$RBUILD_ARTIFACTS_DIR/shizuku.uf2\""]
depends_on = "build"
```

Run a job with `rbuild job build` or `rbuild job run`; `rbuild jobs --json`
lists project jobs. Dependencies run first. Local commands receive
`RBUILD_ARTIFACTS_DIR`, set to the project's `.rbuild-out` directory, so they
can use files collected by an earlier remote build. Remote build jobs collect
normal Bazel/Cargo outputs automatically; `artifacts` can add worktree relative
paths to collect explicitly. Existing `rbuild build ...` commands continue to
use the worker configuration directly.

An explicit `backend` setting wins. Otherwise `MODULE.bazel`, `WORKSPACE`, or root `BUILD` selects Bazel; `Cargo.toml` selects Cargo; other repositories use the generic command backend.

- **Bazel** runs `bazelisk <args>` remotely and retrieves `build` outputs using `bazelisk cquery --output=files`. A cquery path is accepted when it resolves inside the canonical worktree, or inside the canonical `output_path` reported by `bazelisk info output_path`. That root is queried only when a file resolves outside the worktree, for example through a `bazel-out` symlink. Paths under `bazel-out/<config>/bin/` map to `bazel-bin`-relative paths such as `pkg/app`.
- **Cargo** runs `cargo build --message-format=json`. The JSON is parsed by rbuild; only the rendered compiler messages and a `build-finished` summary are printed. Raw JSON is not printed, and stderr and the exit code pass through. By default the retrieved files are executables and `cdylib`/`staticlib`/`dylib` outputs. Build script outputs, `.rlib`/`.rmeta`/`.d` files, `proc-macro` outputs, and dSYM bundles are skipped. `proc-macro` is excluded because it is a compile-time input for dependent crates, not a deliverable. `--artifacts all` retrieves every file cargo reports, including those.
- **Generic** executes the first argument as a remote program and remaining arguments as its argv. Configured `artifact_globs` select output files relative to the remote worktree.

There is no local build fallback. If the remote bazelisk or cargo is missing, the command fails.

Remote PATH: `remote_path_entries` (or `--remote-path-entry`) are prepended to the PATH inherited by the agent, for build, cquery, and generic commands. rbuild does not add `$HOME/.local/bin` or `$HOME/.cargo/bin` by default. The client's `$HOME` is not the remote user's home directory, so the client cannot name those paths correctly, and the agent's PATH depends on the remote login environment. Set `cargo_path` or `remote_path_entries` explicitly when a tool lives in one of those directories.

Build artifacts are placed under `.rbuild-out/`. `compile_commands.json` there has remote-worktree paths rewritten to the local repository root. `rbuild-debug-map.json` contains GDB `set substitute-path` and VS Code cppdbg `sourceFileMap` values.

Unknown Bazel flags whose values cannot be safely separated from target labels cause artifact collection to be skipped with a warning. The build command itself still receives its original arguments.

## Dependency repositories (`[deps.*]`)

For multi-repository setups, project dependencies can be declared in the repository's `rbuild.toml`:

```toml
[deps.shizuku]
path = "../Shizuku-v3"
bazel_module = "shizuku"
```

- **Synchronization**: Each declared dependency is synchronized to the remote worker (`<build_root>/<client_id>/<dep_name>/<worktree_name>`) prior to synchronizing the main repository. Both committed and uncommitted/untracked changes are transferred.
- **Manifest cache isolation**: Dependency manifest caches are stored inside the main project at `.rbuild/deps/<dep_name>_manifest_cache.json`, leaving dependency working trees untouched.
- **Bazel module override**: If `bazel_module` is set, `rbuild` automatically appends `--override_module=<bazel_module>=<worker_path>` immediately after the Bazel subcommand for both build execution and artifact cquery resolution. This avoids depending on local `.bazelrc.user` configurations that are not present on remote workers.
- **Environment variables**: For all backends (Bazel, Cargo, and generic), the worker-side path is passed via environment variables named `RBUILD_DEP_<NAME>` (normalized to uppercase with non-alphanumeric characters replaced with `_`).


## Protocol and filesystem behavior

Frames use a 4-byte big-endian length and a 64 MiB maximum serialized payload. Artifact files are sent as 1 MiB raw chunks, zstd-compressed independently, then checked for offset, length, and BLAKE3 before atomic placement. Workspace sync files still use one frame per file and therefore must fit the frame limit.

Workspace enumeration uses `git ls-files -co --exclude-standard`; non-Git source directories are not supported. Git submodules (gitlinks) are skipped with a warning. Non-regular entries and entries that cannot be inspected or read are skipped with a path-specific warning. It preserves executable bits and symlink entries, rejects path traversal and escaping symlinks, and uses BLAKE3 metadata caching plus whole-file zstd compression. Deleted files are removed from the remote worktree.

Wire compatibility: `PROTOCOL_VERSION` (currently 3) must match exactly on both sides. The agent answers `compatible=false` to any other version, and the client also refuses a `compatible=true` answer that reports a different version. Protocol 2 added `CollectArtifactsRequest.envs` and the reserved `RBUILD_PATH_PREPEND` env key, so an agent on protocol 1 would silently ignore PATH entries. Protocol 3: artifact output-root query receives `--override_module` flags. The handshake therefore fails closed in every mismatch case, and no lock or build request is sent.

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
