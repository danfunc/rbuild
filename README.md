# rbuild

Remote Bazel build execution and workspace synchronization tool written in Rust.

`rbuild` runs as a single binary acting as either the local client (`rbuild <bazel args...>`) or the remote agent daemon (`rbuild agent`). It syncs the local workspace into a remote build worktree without invoking Bazel on the client, executes build commands exclusively on the agent, and streams stdout/stderr back in real time.

---

## Architecture & Design

### Single Binary & Modes
- **Client mode**: `rbuild [OPTIONS] [BAZEL_ARGS]...`
  - Scans tracked and untracked repository files via `git ls-files -co --exclude-standard`.
  - Computes BLAKE3 hashes incrementally using a local `.rbuild/manifest_cache.json` cache (keyed on `mtime` + `size`).
  - Connects to the agent daemon, checks version/build ID compatibility, and initiates agent binary update if mismatched.
  - Locks the target worktree exclusively (`.rbuild.lock`).
  - Transfers differential files using whole-file zstd compression and BLAKE3 verification.
  - Instructs the agent to execute remote Bazel commands. The client **never** invokes Bazel locally.
- **Agent mode**: `rbuild agent [--base-dir /srv/build] [--bin-path <PATH>]`
  - Listens on standard I/O streams using length-prefixed protocol frames.
  - Manages worktrees organized as `<base_dir>/<client>/<repo>/<worktree>`.
  - Enforces path traversal prevention and symlink escape validation.
  - Applies file updates atomically via temporary sibling files and renames.
  - Executes build commands and forwards stdout/stderr frames back to the client.

### Framing & Protocol
- **Framing**:
  - 4-byte big-endian `u32` length prefix.
  - Strictly bounded frame size (`MAX_FRAME_SIZE = 64 MiB`) to prevent unbounded memory allocation.
  - Standard JSON-serialized payload (`serde_json`).
  - Standard output/error of child commands are encapsulated in dedicated `CommandStdout` and `CommandStderr` protocol frames; raw child process stdout never mixes with protocol stream.
  - **Single-Frame Payload & Artifact Size Limit**:
    - Current protocol transfers both sync files (`SyncFileChunk`) and build artifacts (`ArtifactChunk`) as whole files in a single frame.
    - Due to JSON serialization expansion of byte arrays (up to ~3-4x in worst-case uncompressed/high-entropy payloads), single-file artifact sizes are explicitly capped at `MAX_ARTIFACT_SIZE = 16 MiB` (`src/agent/mod.rs`). Artifacts exceeding this size are rejected with an explicit error to prevent exceeding `MAX_FRAME_SIZE`.
- **Agent Binary Replacement**:
  - During handshake, if the agent reports an incompatible build ID or protocol version, the client transmits its current binary (zstd-compressed).
  - The agent verifies the raw length and BLAKE3 hash, writes the binary to a temporary file, sets executable permissions (`0o755`), and atomically renames it over the target agent binary path.

### Workspace Synchronization & Semantics
- **File Detection**:
  - Uses `git ls-files -co --exclude-standard` to enumerate tracked and untracked repository files.
  - **Current Requirement**: The source root **must** be a Git repository (`src/sync/manifest.rs`). Standalone non-git directory traversal fallback is not yet implemented.
  - Skips `.git`, `.rbuild`, and user-defined exclude patterns (`extra_excludes`).
- **Executable Bit (`mode`)**:
  - Unix file mode permission executable bits (`0o111`) are preserved.
  - Regular files with executable bits are saved with `0o755` permissions; regular non-executable files default to `0o644`.
- **Symlink Handling & Path Safety**:
  - Symlinks are transferred as symlink records containing their link target path (rather than following the symlink).
  - Escaping symlinks (absolute targets escaping base directory, or relative paths with `..` escaping base directory) are strictly rejected both in workspace sync and in post-build artifact collection (`resolve_artifact_file` canonicalizes paths and validates worktree containment).
  - On the remote agent, symlinks are created using `std::os::unix::fs::symlink`.
- **Path Traversal Defense**:
  - Relative paths containing `..`, absolute paths (`/`), null bytes (`\0`), and paths escaping the allocated worktree root are rejected both on the client and on the agent.
  - Deletions are strictly confined within the allocated worktree root.
- **Atomic File Writing**:
  - Incoming files are written to `.rbuild_sync_tmp_*` in the destination directory, verified against raw length and BLAKE3 checksum, and atomically renamed.
  - Corruption or hash mismatches cause immediate rejection without damaging target files.

---

## Configuration (`~/.config/rbuild.toml`)

`rbuild` reads its configuration from `~/.config/rbuild.toml` (with built-in defaults fallback if the file does not exist):

```toml
# Transport: "local" (child process pipe) is currently supported.
# "ssh" (external OpenSSH command) is not yet supported in this unit.
transport = "local"

# Remote host for SSH transport (reserved for future SSH support)
remote = "build-server"

# Base directory on remote build host
build_root = "/srv/build"

# Client identifier (defaults to $USER)
client_id = "developer"

# Additional relative path prefix patterns to exclude from synchronization
extra_excludes = [
    "target/",
    "build-temp/",
]
```

CLI flags (e.g. `--transport`, `--build-root`, `--remote`) override settings in `rbuild.toml`.
Note: Specifying `--transport=ssh` will currently return an error as SSH transport is not yet implemented.

---

## Current Status & Implemented Features

- [x] Cargo project scaffolding with Rust 2024 edition.
- [x] CLI structure supporting client and `agent` subcommand (`clap`).
- [x] Length-prefixed framing with 64 MiB bounded frame limit (`src/protocol/frame.rs`).
- [x] Request/Response protocol definitions (`src/protocol/message.rs`).
- [x] BLAKE3 hashing & incremental manifest caching (`src/sync/manifest.rs`).
- [x] Atomic agent binary update with checksum validation and chmod `0o755` (`src/agent/updater.rs`).
- [x] Exclusive worktree lock mechanism (`src/agent/lock.rs`).
  *(Limitation: Lock file deletion relies on `Drop`; process crashes leave stale lock files requiring manual deletion)*.
- [x] Whole-file zstd compression & decompression (`src/sync/apply.rs`).
- [x] Differential sync calculation (`src/sync/diff.rs`).
- [x] Path traversal and symlink escape defenses (`src/sync/path_safety.rs`).
- [x] Local transport via piped child process and unix stream (`src/client/transport.rs`).
- [x] Artifact collection: post-build collection via `bazelisk cquery` and streaming artifacts into `.rbuild-out/` (`src/client/mod.rs`, `src/agent/mod.rs`).
  - Strict worktree confinement via `fs::canonicalize` to reject symlinks escaping the worktree root.
  - Bounded artifact size limit (`MAX_ARTIFACT_SIZE = 16 MiB`) to prevent protocol frame overflow.
- [x] Bazel argument parsing with separate value flags (`-c`, `--jobs`, `--config`, etc.) and ambiguity detection (`src/main.rs`).
  - Safe behavior: If unknown flags followed by non-flag arguments are encountered, target classification is marked ambiguous and artifact collection is safely skipped to prevent corrupted queries.
- [x] Comprehensive test suite (28 unit tests + 16 integration tests, passing).

---

## Unimplemented / Subsequent Unit Roadmap

1. **OpenSSH Transport**:
   - Spawning external OpenSSH (`ssh <remote> rbuild agent ...`) using standard `~/.ssh/config`.
   - Forwarding standard I/O pipes over SSH.
2. **Non-Git Workspace Traversal**:
   - Clean filesystem traversal fallback when running in directories not managed by Git.
3. **`compile_commands.json` Path Rewriting**:
   - Rewriting remote worktree paths to local workspace paths in Bazel compilation databases.
4. **Lock Liveness & Worktree Cleanup**:
   - Automatic recovery of stale `.rbuild.lock` files from dead processes (PID liveness or OS `flock`).
   - Retention policy and garbage collection for stale remote worktrees.
5. **Cross-Compilation for Linux Agent (`x86_64-unknown-linux-musl`)**:
   - **Target Status**: `x86_64-unknown-linux-musl` is a standard Rust Tier 2 target ([Rust Platform Support](https://doc.rust-lang.org/rustc/platform-support.html)).
   - **Installation Requirement**: Requires standard library components via `rustup target add x86_64-unknown-linux-musl` ([Rustup Documentation](https://rust-lang.github.io/rustup/cross-compilation.html)).
   - **Linker Requirement**: macOS hosts cannot link musl ELF binaries with standard macOS clang/ld. An external musl cross-linker (e.g. `x86_64-linux-musl-gcc` via `musl-cross`) or `cargo-zigbuild` ([cargo-zigbuild Project](https://github.com/rust-cross/cargo-zigbuild)) using Zig as the linker is required.
   - **Recommended Build Command (with zigbuild)**:
     ```bash
     cargo zigbuild --target x86_64-unknown-linux-musl --bin rbuild --release
     ```
   - *(Note: Cross-compilation research verified via official docs; cross-toolchain installation not performed on this machine)*.
