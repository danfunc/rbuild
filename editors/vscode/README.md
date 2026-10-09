# rbuild for VS Code

This extension runs the local `rbuild` CLI in the first workspace folder. Configure the CLI's transport and remote settings in `~/.config/rbuild.toml`; the extension does not implement a separate transport. It passes the configured argument array as individual process arguments. Keep a build subcommand/target in the array: rbuild with no build arguments performs synchronization only.

## Commands and keys

- **rbuild: Build**: status bar `rbuild`, editor title button, or `rbuild.build`.
- **rbuild: Run**: status bar play button or `rbuild.run`; uses the project's `run` job when defined.
- **rbuild: Build and Debug**: status bar `rbuild`, editor title button, or `rbuild.debug`.
- When `rbuild.toml` defines the corresponding job, Build invokes `rbuild job build` and Build and Debug builds that job before launching. Without a project job, these commands use `rbuild.arguments` as before.
- macOS: `Cmd+Option+B` builds; `Cmd+Option+D` builds and starts debugging.
- macOS: `Cmd+Option+R` runs.
- Windows/Linux: `Ctrl+Alt+B` builds; `Ctrl+Alt+D` builds and starts debugging.
- Windows/Linux: `Ctrl+Alt+R` runs.
- **rbuild: Cancel Build** (`rbuild.cancel`) stops the running local CLI process.

Each run clears the Output channel and prints CMake Tools style lines: `[sync]`, `[build]`, `[fetch]` and `[run]` prefixes, `[job:<name>] Started (n/m)` headers for multi-stage jobs, `[sync] up to date` when nothing changed, and a closing `[build] Build finished with exit code N (12.3s)`. A notification (with Cancel) and the status bar show the stage and an approximate percentage: sync and fetch use transfer counts from `rbuild --progress=json`; the build stage estimates from Bazel `[370 / 409]` or cargo `Building [==>] 120/300` lines (stage name only when neither appears). Fetch totals are not known in advance, so it shows files and bytes without a percentage.

The extension prevents a second concurrent invocation. Output goes to the `rbuild` Output channel. Build errors are reported with the CLI exit status. Error lines appear in Output; this extension does not register a problem matcher because rbuild's output can be Cargo, Bazel, or generic and does not provide a stable unified diagnostics format.

## Settings

| Setting | Default | Meaning |
| --- | --- | --- |
| `rbuild.executable` | `rbuild` | Local rbuild executable path. |
| `rbuild.arguments` | `["build"]` | Argument vector. Example Bazel: `["build", "//pkg:target"]`; Cargo: `["build", "--release"]`; generic: `["./scripts/build.sh"]`. |
| `rbuild.backend` | `""` | Optional `bazel`, `cargo`, or `generic` override, passed as `--backend=...`. |
| `rbuild.debug.launchConfiguration` | `""` | Name of a configuration already present in `.vscode/launch.json`. |
| `rbuild.debug.sourceMapProperty` | `sourceFileMap` | Property to receive rbuild's remote-to-local root mapping; `pathSubstitutions` is available for adapters using that key, or `none`. |

The CLI's own `~/.config/rbuild.toml` controls transport, remote, remote agent, build root, artifact globs, and tool paths. The extension does not synthesize CLI options for these settings.

## Debug launch and path mapping

Debug first runs the configured build command, which must succeed and collect artifacts. rbuild writes `.rbuild-out/rbuild-debug-map.json`; the extension reads its `remote_root` and `local_root`, merges that pair into the selected launch configuration's configured mapping property, then calls `vscode.debug.startDebugging` with the configuration name. The launch configuration in `.vscode/launch.json` is updated in the workspace folder so VS Code can resolve the mapping. The remote executable path/target and debugger server remain the user's launch configuration responsibility.

For Microsoft `cppdbg`, the documented mapping key is `sourceFileMap`; the default works. For Cortex-Debug, verify which mapping property the installed version accepts; select `pathSubstitutions` only if that adapter version uses that exact key. For CodeLLDB, check whether `sourceMap` is required; this extension does not currently expose that property. Adapter configuration keys vary and this extension does not detect the debugger type. If the map file is absent (for example no artifacts were collected), it warns and starts the launch configuration without injecting a mapping.

Example `.vscode/settings.json` for a Shizuku Bazel target:

```json
{
  "rbuild.executable": "/Users/me/.cargo/bin/rbuild",
  "rbuild.arguments": ["build", "//firmware:esp32c6"],
  "rbuild.backend": "bazel",
  "rbuild.debug.launchConfiguration": "Shizuku ESP32-C6 (OpenOCD)",
  "rbuild.debug.sourceMapProperty": "sourceFileMap"
}
```

Example `.vscode/launch.json` (keep the `cortex-debug` fields appropriate for the board and installed extension):

```json
{
  "version": "0.2.0",
  "configurations": [
    {
      "name": "Shizuku ESP32-C6 (OpenOCD)",
      "type": "cortex-debug",
      "request": "launch",
      "servertype": "openocd",
      "executable": "${workspaceFolder}/.rbuild-out/firmware/firmware.elf",
      "configFiles": ["interface/cmsis-dap.cfg", "target/esp32c6.cfg"],
      "sourceFileMap": {}
    }
  ]
}
```

The example artifact path and OpenOCD files are project-specific; match them to actual Bazel outputs and the local OpenOCD installation. rbuild does not rewrite the executable's debug information or configure/launch OpenOCD itself. Its mapping covers source roots reported in the debug map.

## Build and install locally

From this directory run `npm install`, `npm run compile`, `npm test`, then `npx vsce package`. Install the resulting `local.rbuild-*.vsix` with `code --install-extension local.rbuild-*.vsix`.
