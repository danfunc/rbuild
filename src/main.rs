use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rbuild::agent::run_agent_loop;
use rbuild::client::ClientSession;
use rbuild::client::transport::LocalProcessTransport;
use rbuild::config::Config;
use rbuild::sync::manifest::ManifestCache;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(name = "rbuild")]
#[command(
    about = "Remote Bazel build execution and workspace synchronization tool",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(long, help = "Transport method: 'local' or 'ssh'")]
    transport: Option<String>,

    #[arg(long, help = "Remote SSH host")]
    remote: Option<String>,

    #[arg(long, help = "Remote base build directory (e.g. /srv/build)")]
    build_root: Option<String>,

    #[arg(long, help = "Client identifier (e.g. hostname or username)")]
    client_id: Option<String>,

    #[arg(long, help = "Repository name")]
    repo: Option<String>,

    #[arg(long, help = "Worktree branch / name")]
    worktree: Option<String>,

    #[arg(long, help = "Path to custom configuration file")]
    config: Option<PathBuf>,

    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        help = "Bazel arguments passed to remote agent"
    )]
    bazel_args: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum Commands {
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

    println!(
        "rbuild: Connecting via transport '{}' (client='{}', repo='{}', worktree='{}')",
        transport_mode, client_id, repo_name, worktree_name
    );

    match transport_mode.as_str() {
        "local" => {
            let current_exe = std::env::current_exe().context("Locating current executable")?;
            let transport =
                LocalProcessTransport::spawn(&current_exe, Some(Path::new(&build_root)))?;
            let mut session = ClientSession::new(transport);

            // Handshake
            session.handshake(Some(&current_exe))?;
            println!("rbuild: Handshake successful.");

            // Lock Worktree
            let remote_path = session.lock_worktree(&client_id, &repo_name, &worktree_name)?;
            println!("rbuild: Worktree locked at '{}'", remote_path);

            // Sync
            let cache_path = current_dir.join(".rbuild/manifest_cache.json");
            let mut cache = ManifestCache::load_from_file(&cache_path);

            println!("rbuild: Synchronizing workspace...");
            let stats = session.sync_workspace(&current_dir, &mut cache, &cfg.extra_excludes)?;
            let _ = cache.save_to_file(&cache_path);

            println!(
                "rbuild: Sync complete (checked {}, transferred {}, deleted {})",
                stats.files_checked, stats.files_transferred, stats.files_deleted
            );

            // If bazel args provided, run on agent (NEVER locally)
            if !cli.bazel_args.is_empty() {
                let parsed = parse_bazel_args(&cli.bazel_args);

                println!(
                    "rbuild: Executing remote bazelisk command: {:?}",
                    cli.bazel_args
                );
                let exit_code = session.run_remote_command("bazelisk", &cli.bazel_args, &[])?;
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
                        let count = session.collect_artifacts(
                            &out_dir,
                            None,
                            &parsed.targets,
                            &parsed.flags,
                        )?;
                        println!("rbuild: Successfully collected {} artifact(s).", count);
                    }
                }
            }

            Ok(())
        }
        "ssh" => {
            anyhow::bail!("SSH transport not yet supported in this unit. Use --transport=local");
        }
        other => {
            anyhow::bail!("Unsupported transport '{}'. Use 'local' or 'ssh'", other);
        }
    }
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
}
