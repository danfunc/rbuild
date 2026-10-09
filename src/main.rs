use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rbuild::agent::run_agent_loop;
use rbuild::backend::{BackendKind, select_backend};
use rbuild::client::ClientSession;
use rbuild::client::transport::{LocalProcessTransport, SshTransport, Transport};
use rbuild::config::Config;
use rbuild::sync::manifest::ManifestCache;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(name = "rbuild")]
#[command(
    about = "Remote Bazel build execution and workspace synchronization tool",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    #[arg(long, help = "Transport method: 'local' or 'ssh'")]
    pub transport: Option<String>,

    #[arg(long, help = "Remote SSH destination (e.g. user@host)")]
    pub remote: Option<String>,

    #[arg(
        long,
        help = "Remote agent binary path (e.g. rbuild or /usr/local/bin/rbuild)"
    )]
    pub agent_path: Option<String>,

    #[arg(long, help = "Build backend override: bazel, cargo, or generic")]
    pub backend: Option<String>,

    #[arg(long, help = "Remote base build directory (e.g. /srv/build)")]
    pub build_root: Option<String>,

    #[arg(long, help = "Client identifier (e.g. hostname or username)")]
    pub client_id: Option<String>,

    #[arg(long, help = "Repository name")]
    pub repo: Option<String>,

    #[arg(long, help = "Worktree branch / name")]
    pub worktree: Option<String>,

    #[arg(long, help = "Path to custom configuration file")]
    pub config: Option<PathBuf>,

    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        help = "Bazel arguments passed to remote agent"
    )]
    pub bazel_args: Vec<String>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run as remote agent daemon
    Agent {
        #[arg(long, default_value = "/srv/build", help = "Base directory for builds")]
        base_dir: PathBuf,

        #[arg(long, help = "Path to agent binary to update if version mismatches")]
        bin_path: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Agent { base_dir, bin_path }) => {
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            run_agent_loop(stdin.lock(), stdout.lock(), base_dir, bin_path)
        }
        None => run_client(cli),
    }
}

fn run_client(cli: Cli) -> Result<()> {
    let cfg = Config::load_or_default(cli.config.as_deref());

    let transport_mode = cli.transport.unwrap_or(cfg.transport);
    let remote = cli.remote.or(cfg.remote);
    let agent_path = cli.agent_path.unwrap_or(cfg.agent_path);
    let build_root = cli.build_root.unwrap_or(cfg.build_root);

    let client_id = cli.client_id.or(cfg.client_id).unwrap_or_else(|| {
        std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "client".to_string())
    });

    let current_dir = std::env::current_dir().context("Getting current working directory")?;
    let repo_name = cli.repo.unwrap_or_else(|| {
        current_dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "repo".to_string())
    });

    let worktree_name = cli.worktree.unwrap_or_else(|| "main".to_string());
    let backend = select_backend(
        &current_dir,
        cli.backend.as_deref().or(cfg.backend.as_deref()),
    )?;

    println!(
        "rbuild: Connecting via transport '{}' (client='{}', repo='{}', worktree='{}')",
        transport_mode, client_id, repo_name, worktree_name
    );

    match transport_mode.as_str() {
        "local" => {
            let current_exe = std::env::current_exe().context("Locating current executable")?;
            let transport =
                LocalProcessTransport::spawn(&current_exe, Some(Path::new(&build_root)))?;
            let session = ClientSession::new(transport);
            run_session_workflow(
                session,
                Some(&current_exe),
                &current_dir,
                &client_id,
                &repo_name,
                &worktree_name,
                &cfg.excludes,
                backend,
                &cfg.artifact_globs,
                &cli.bazel_args,
            )
        }
        "ssh" => {
            let dest = remote.ok_or_else(|| {
                anyhow::anyhow!(
                    "SSH transport requires a remote destination. Specify via --remote or 'remote' in config"
                )
            })?;
            let transport = SshTransport::spawn(&dest, &agent_path, Some(&build_root))?;
            let session = ClientSession::new(transport);
            // In cross-platform environments (e.g. macOS client -> Linux agent), local binary must not be uploaded
            run_session_workflow(
                session,
                None,
                &current_dir,
                &client_id,
                &repo_name,
                &worktree_name,
                &cfg.excludes,
                backend,
                &cfg.artifact_globs,
                &cli.bazel_args,
            )
        }
        other => {
            anyhow::bail!("Unsupported transport '{}'. Use 'local' or 'ssh'", other);
        }
    }
}

fn run_session_workflow<T: Transport>(
    mut session: ClientSession<T>,
    local_exe_for_update: Option<&Path>,
    current_dir: &Path,
    client_id: &str,
    repo_name: &str,
    worktree_name: &str,
    excludes: &[String],
    backend: BackendKind,
    artifact_globs: &[String],
    bazel_args: &[String],
) -> Result<()> {
    // Handshake
    session.handshake(local_exe_for_update)?;
    println!("rbuild: Handshake successful.");

    // Lock Worktree
    let remote_path = session.lock_worktree(client_id, repo_name, worktree_name)?;
    println!("rbuild: Worktree locked at '{}'", remote_path);

    // Sync
    let cache_path = current_dir.join(".rbuild/manifest_cache.json");
    let mut cache = ManifestCache::load_from_file(&cache_path);

    println!("rbuild: Synchronizing workspace...");
    let stats = session.sync_workspace(current_dir, &mut cache, excludes)?;
    let _ = cache.save_to_file(&cache_path);

    println!(
        "rbuild: Sync complete (checked {}, transferred {}, deleted {})",
        stats.files_checked, stats.files_transferred, stats.files_deleted
    );

    // If bazel args provided, run on agent (NEVER locally)
    if !bazel_args.is_empty() {
        if backend == BackendKind::Cargo {
            let mut args = if bazel_args.first().is_some_and(|s| s == "build") {
                bazel_args[1..].to_vec()
            } else {
                bazel_args.to_vec()
            };
            args.retain(|a| !a.starts_with("--message-format="));
            args.splice(
                0..0,
                ["build".to_string(), "--message-format=json".to_string()],
            );
            let (code, stdout, _) = session.run_remote_command_capture("cargo", &args, &[])?;
            if code != 0 {
                std::process::exit(code);
            }
            let paths = cargo_artifact_paths(&stdout, Path::new(&remote_path));
            println!("rbuild: Cargo reported {} artifact path(s).", paths.len());
            if !paths.is_empty() {
                let output = current_dir.join(".rbuild-out");
                session.collect_paths(&output, &paths)?;
                finalize_debug_artifacts(&output, &remote_path, current_dir)?;
            }
            return Ok(());
        }
        if backend == BackendKind::Generic {
            let (program, args) = bazel_args
                .split_first()
                .ok_or_else(|| anyhow::anyhow!("Generic backend requires a command"))?;
            let code = session.run_remote_command(program, args, &[])?;
            if code != 0 {
                std::process::exit(code);
            }
            if !artifact_globs.is_empty() {
                let out = current_dir.join(".rbuild-out");
                session.collect_paths(&out, artifact_globs)?;
                finalize_debug_artifacts(&out, &remote_path, current_dir)?;
            }
            return Ok(());
        }
        let parsed = parse_bazel_args(bazel_args);

        println!(
            "rbuild: Executing remote bazelisk command: {:?}",
            bazel_args
        );
        let exit_code = session.run_remote_command("bazelisk", bazel_args, &[])?;
        if exit_code != 0 {
            std::process::exit(exit_code);
        }

        // If this was a build command, collect build artifacts into .rbuild-out
        if parsed.subcommand.as_deref() == Some("build") {
            if parsed.is_ambiguous {
                eprintln!(
                    "rbuild: Warning: Build target list cannot be unambiguously determined; skipping artifact collection."
                );
            } else if parsed.targets.is_empty() {
                println!(
                    "rbuild: Skipping artifact collection: no explicit build targets specified."
                );
            } else {
                let out_dir = current_dir.join(".rbuild-out");
                println!(
                    "rbuild: Collecting build artifacts into '{}'...",
                    out_dir.display()
                );
                let count =
                    session.collect_artifacts(&out_dir, None, &parsed.targets, &parsed.flags)?;
                println!("rbuild: Successfully collected {} artifact(s).", count);
                finalize_debug_artifacts(&out_dir, &remote_path, current_dir)?;
            }
        }
    }

    Ok(())
}

fn finalize_debug_artifacts(out_dir: &Path, remote_root: &str, local_root: &Path) -> Result<()> {
    let local = local_root.to_string_lossy();
    rbuild::debug_map::rewrite_compile_commands(
        &out_dir.join("compile_commands.json"),
        remote_root,
        &local,
    )?;
    rbuild::debug_map::write_debug_map(out_dir, remote_root, &local)?;
    Ok(())
}

fn cargo_artifact_paths(json_lines: &[u8], remote_root: &Path) -> Vec<String> {
    let mut paths = std::collections::BTreeSet::new();
    for line in json_lines.split(|b| *b == b'\n') {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("reason").and_then(|v| v.as_str()) != Some("compiler-artifact") {
            continue;
        }
        if let Some(exe) = value.get("executable").and_then(|v| v.as_str()) {
            if let Some(p) = relativize_remote_path(Path::new(exe), remote_root) {
                paths.insert(p);
            }
        }
        if let Some(filenames) = value.get("filenames").and_then(|v| v.as_array()) {
            for filename in filenames.iter().filter_map(|v| v.as_str()) {
                if let Some(p) = relativize_remote_path(Path::new(filename), remote_root) {
                    paths.insert(p);
                }
            }
        }
    }
    paths.into_iter().collect()
}

fn relativize_remote_path(path: &Path, root: &Path) -> Option<String> {
    if let Ok(rel) = path.strip_prefix(root) {
        return Some(rel.to_string_lossy().to_string());
    }
    // Some Unix systems expose the same path through aliases such as /private/var and /var.
    let root_parts: Vec<_> = root
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let path_parts: Vec<_> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    for start in 0..root_parts.len() {
        let suffix = &root_parts[start..];
        if suffix.len() < 2 || suffix.len() > path_parts.len() {
            continue;
        }
        for i in 0..=path_parts.len() - suffix.len() {
            if path_parts[i..i + suffix.len()] == *suffix {
                let rest = &path_parts[i + suffix.len()..];
                if !rest.is_empty() {
                    return Some(rest.join("/"));
                }
            }
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedBazelArgs {
    pub subcommand: Option<String>,
    pub targets: Vec<String>,
    pub flags: Vec<String>,
    pub is_ambiguous: bool,
}

/// Checks whether a given command line flag expects a separate following value argument.
fn is_known_value_flag(flag: &str) -> bool {
    matches!(
        flag,
        "-c" | "--compilation_mode"
            | "-j"
            | "--jobs"
            | "--output"
            | "--config"
            | "--platforms"
            | "--cpu"
            | "--host_cpu"
            | "--crosstool_top"
            | "--compiler"
            | "--action_env"
            | "--define"
            | "--aspects"
            | "--build_tag_filters"
            | "--test_tag_filters"
            | "--target_pattern_file"
            | "--override_repository"
            | "--package_path"
            | "--distdir"
            | "--repository_cache"
            | "--symlink_prefix"
            | "--sandbox_tmpfs_path"
            | "--test_arg"
            | "--test_filter"
            | "--run_under"
            | "--strategy"
            | "--genrule_strategy"
            | "--spawn_strategy"
            | "--copt"
            | "--cxxopt"
            | "--linkopt"
            | "--host_copt"
            | "--host_cxxopt"
            | "--host_linkopt"
    )
}

pub fn parse_bazel_args(args: &[String]) -> ParsedBazelArgs {
    let mut subcommand = None;
    let mut targets = Vec::new();
    let mut flags = Vec::new();
    let mut is_ambiguous = false;

    let mut past_subcommand = false;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        if arg == "--" {
            // Everything after -- is unambiguously a target (or arguments to targets)
            i += 1;
            while i < args.len() {
                targets.push(args[i].clone());
                i += 1;
            }
            break;
        }

        if !past_subcommand {
            if !arg.starts_with('-') {
                subcommand = Some(arg.clone());
                past_subcommand = true;
                i += 1;
                continue;
            } else {
                // Startup option before subcommand
                if is_known_value_flag(arg) {
                    flags.push(arg.clone());
                    if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                        flags.push(args[i + 1].clone());
                        i += 2;
                        continue;
                    }
                }
                flags.push(arg.clone());
                i += 1;
                continue;
            }
        }

        // Past subcommand
        if arg.starts_with('-') {
            if arg.contains('=') {
                flags.push(arg.clone());
                i += 1;
            } else if is_known_value_flag(arg) {
                flags.push(arg.clone());
                if i + 1 < args.len() && args[i + 1] != "--" {
                    flags.push(args[i + 1].clone());
                    i += 2;
                } else {
                    // Flag expected value but none given before end/--
                    is_ambiguous = true;
                    i += 1;
                }
            } else {
                flags.push(arg.clone());
                // If next token does not start with '-' and is not '--',
                // we cannot know if it's an argument to this unknown flag or a target.
                if i + 1 < args.len() && !args[i + 1].starts_with('-') && args[i + 1] != "--" {
                    is_ambiguous = true;
                }
                i += 1;
            }
        } else {
            targets.push(arg.clone());
            i += 1;
        }
    }

    ParsedBazelArgs {
        subcommand,
        targets,
        flags,
        is_ambiguous,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bazel_args() {
        let args = vec![
            "build".to_string(),
            "//pkg:app".to_string(),
            "--compilation_mode=opt".to_string(),
            "//other:lib".to_string(),
        ];
        let parsed = parse_bazel_args(&args);
        assert_eq!(parsed.subcommand.as_deref(), Some("build"));
        assert_eq!(parsed.targets, vec!["//pkg:app", "//other:lib"]);
        assert_eq!(parsed.flags, vec!["--compilation_mode=opt"]);
        assert!(!parsed.is_ambiguous);

        // With separate value flag -c and double dash
        let args_dd = vec![
            "build".to_string(),
            "-c".to_string(),
            "opt".to_string(),
            "--".to_string(),
            "-special_target".to_string(),
        ];
        let parsed_dd = parse_bazel_args(&args_dd);
        assert_eq!(parsed_dd.subcommand.as_deref(), Some("build"));
        assert_eq!(parsed_dd.targets, vec!["-special_target"]);
        assert_eq!(parsed_dd.flags, vec!["-c", "opt"]);
        assert!(!parsed_dd.is_ambiguous);

        // With common flags that take separate values
        let args_common = vec![
            "build".to_string(),
            "--config".to_string(),
            "release".to_string(),
            "--jobs".to_string(),
            "8".to_string(),
            "--platforms".to_string(),
            "//platforms:linux".to_string(),
            "//server:bin".to_string(),
        ];
        let parsed_common = parse_bazel_args(&args_common);
        assert_eq!(parsed_common.subcommand.as_deref(), Some("build"));
        assert_eq!(parsed_common.targets, vec!["//server:bin"]);
        assert_eq!(
            parsed_common.flags,
            vec![
                "--config",
                "release",
                "--jobs",
                "8",
                "--platforms",
                "//platforms:linux"
            ]
        );
        assert!(!parsed_common.is_ambiguous);

        // Ambiguous unknown flag followed by non-hyphen argument
        let args_ambig = vec![
            "build".to_string(),
            "--unknown-custom-option".to_string(),
            "custom_value".to_string(),
            "//pkg:app".to_string(),
        ];
        let parsed_ambig = parse_bazel_args(&args_ambig);
        assert!(parsed_ambig.is_ambiguous);
    }

    #[test]
    fn test_cli_parsing_ssh_transport_flags() {
        let cli = Cli::try_parse_from([
            "rbuild",
            "--transport",
            "ssh",
            "--remote",
            "ishigakiyua@100.64.0.12",
            "--agent-path",
            "/home/ishigakiyua/.cargo/bin/rbuild",
            "--build-root",
            "/srv/build",
            "--client-id",
            "mac-client",
            "build",
            "//pkg:target",
        ])
        .unwrap();

        assert_eq!(cli.transport.as_deref(), Some("ssh"));
        assert_eq!(cli.remote.as_deref(), Some("ishigakiyua@100.64.0.12"));
        assert_eq!(
            cli.agent_path.as_deref(),
            Some("/home/ishigakiyua/.cargo/bin/rbuild")
        );
        assert_eq!(cli.build_root.as_deref(), Some("/srv/build"));
        assert_eq!(cli.client_id.as_deref(), Some("mac-client"));
        assert_eq!(cli.bazel_args, vec!["build", "//pkg:target"]);
    }

    #[test]
    fn cargo_json_artifacts_are_rebased_to_worktree_relative_paths() {
        let root = Path::new("/srv/build/client/repo/main");
        let json = br#"{"reason":"compiler-artifact","filenames":["/srv/build/client/repo/main/target/release/rbuild.d","/srv/build/client/repo/main/target/release/rbuild"],"executable":"/srv/build/client/repo/main/target/release/rbuild"}"#;
        let paths = cargo_artifact_paths(json, root);
        assert!(paths.contains(&"target/release/rbuild".to_string()));
        assert!(paths.contains(&"target/release/rbuild.d".to_string()));
        assert!(!paths.iter().any(|p| p.starts_with('/')));
    }
}
