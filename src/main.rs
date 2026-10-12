use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rbuild::agent::run_agent_loop;
use rbuild::backend::{BackendKind, inject_bazel_override_modules, select_backend};
use rbuild::cargo_output::{ArtifactSelection, artifact_paths, render_human_output};
use rbuild::client::ClientSession;
use rbuild::client::transport::{
    LocalProcessTransport, SshTransport, Transport, build_and_install_native_agent, probe_ssh,
};
use rbuild::config::{
    Config, ProjectConfig, ResolvedDep, WorkerConfig, build_dep_command_envs,
    choose_reachable_worker, job_execution_order, resolve_and_validate_deps, resolve_job_worker,
};
use rbuild::progress::{self, Progress};
use rbuild::protocol::message::path_prepend_envs;
use rbuild::sync::manifest::ManifestCache;
use std::fs;
use std::io::Write;
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

    #[arg(
        long,
        help = "Remote bazelisk executable: a name on the remote PATH or an absolute path"
    )]
    pub bazelisk_path: Option<String>,

    #[arg(
        long,
        help = "Remote cargo executable: a name on the remote PATH or an absolute path"
    )]
    pub cargo_path: Option<String>,

    #[arg(
        long = "remote-path-entry",
        help = "Directory prepended to the remote PATH of build commands (repeatable; replaces configured entries)"
    )]
    pub remote_path_entry: Vec<String>,

    #[arg(
        long,
        value_parser = ["final", "all"],
        help = "Cargo files to collect: 'final' (executables and cdylib/staticlib, default) or 'all'"
    )]
    pub artifacts: Option<String>,

    #[arg(long, help = "Repository name")]
    pub repo: Option<String>,

    #[arg(long, help = "Worktree branch / name")]
    pub worktree: Option<String>,

    #[arg(long, help = "Path to custom configuration file")]
    pub config: Option<PathBuf>,

    #[arg(long, help = "Select a named worker from configuration")]
    pub worker: Option<String>,

    #[arg(
        long,
        value_parser = ["json"],
        help = "Emit machine-readable progress lines on stderr (must precede other arguments)"
    )]
    pub progress: Option<String>,

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
    /// List configured workers as text or JSON
    Workers {
        #[arg(long)]
        json: bool,
    },
    /// Execute a project job
    Job { name: String },
    /// List project jobs
    Jobs {
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.progress.is_some() {
        progress::enable();
    }

    match cli.command {
        Some(Commands::Agent { base_dir, bin_path }) => {
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            run_agent_loop(stdin.lock(), stdout.lock(), base_dir, bin_path)
        }
        Some(Commands::Workers { json }) => {
            let cfg = Config::load_or_default(cli.config.as_deref());
            let workers = cfg.resolved_workers();
            let default_name = cfg
                .default_worker
                .clone()
                .or_else(|| (workers.len() == 1).then(|| workers[0].name.clone()));
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"default": default_name, "workers": workers} )
                    )?
                );
            } else {
                for w in workers {
                    println!(
                        "{}{}\t{}\t{}",
                        w.name,
                        if default_name.as_deref() == Some(&w.name) {
                            " (default)"
                        } else {
                            ""
                        },
                        w.transport,
                        w.remote.as_deref().unwrap_or("")
                    );
                }
            }
            Ok(())
        }
        Some(Commands::Jobs { json }) => list_jobs(json),
        Some(Commands::Job { name }) => run_job(&name),
        None => run_client(cli),
    }
}

fn project_config() -> Result<ProjectConfig> {
    let path = std::env::current_dir()?.join("rbuild.toml");
    if path.exists() {
        ProjectConfig::load(&path).map_err(anyhow::Error::msg)
    } else {
        Ok(ProjectConfig::default())
    }
}

fn list_jobs(json: bool) -> Result<()> {
    let project = project_config()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&project.jobs)?);
    } else {
        for name in project.jobs.keys() {
            println!("{name}");
        }
    }
    Ok(())
}

fn run_job(name: &str) -> Result<()> {
    let project = project_config()?;
    let user = Config::load_or_default(None);
    let workers = user.resolved_workers();
    let order = job_execution_order(&project, name).map_err(anyhow::Error::msg)?;
    let total = order.len();
    for (index, current) in order.into_iter().enumerate() {
        progress::emit_job(&current, index, total);
        let job = &project.jobs[&current];
        let cwd = std::env::current_dir()?;
        let artifacts_dir = cwd.join(".rbuild-out");
        let worker = resolve_job_worker(job, &workers).map_err(anyhow::Error::msg)?;
        if worker.is_none() {
            let (program, args) = job
                .command
                .split_first()
                .ok_or_else(|| anyhow::anyhow!("Local job '{name}' requires command"))?;
            let status = std::process::Command::new(program)
                .args(args)
                .current_dir(&cwd)
                .env("RBUILD_ARTIFACTS_DIR", &artifacts_dir)
                .status()?;
            if !status.success() {
                anyhow::bail!("Local job '{name}' failed with {status}");
            }
            continue;
        }
        if job.args.is_empty() {
            anyhow::bail!("Remote job '{name}' requires args");
        }
        let exe = std::env::current_exe()?;
        let mut command = std::process::Command::new(exe);
        if progress::enabled() {
            command.arg("--progress=json");
        }
        command.arg("--worker").arg(&job.worker);
        if let Some(backend) = &job.backend {
            command.arg("--backend").arg(backend);
        }
        command
            .args(&job.args)
            .current_dir(&cwd)
            .env("RBUILD_ARTIFACTS_DIR", &artifacts_dir);
        command.env(
            "RBUILD_JOB_ARTIFACTS",
            serde_json::to_string(&job.artifacts)?,
        );
        let status = command.status()?;
        if !status.success() {
            anyhow::bail!("Remote job '{name}' failed with {status}");
        }
    }
    Ok(())
}

fn run_client(cli: Cli) -> Result<()> {
    let cfg = Config::load_or_default(cli.config.as_deref());

    let workers = cfg.resolved_workers();
    let selected_name = cli
        .worker
        .as_deref()
        .or(cfg.default_worker.as_deref())
        .or_else(|| (workers.len() == 1).then(|| workers[0].name.as_str()));
    let selected_worker = selected_name.and_then(|n| workers.iter().find(|w| w.name == n));
    if !workers.is_empty() && selected_worker.is_none() {
        anyhow::bail!(
            "Unknown or unspecified worker; select --worker <name> or configure default_worker"
        );
    }
    let mut selected = selected_worker.cloned().unwrap_or_else(|| WorkerConfig {
        name: "default".into(),
        transport: cfg.transport.clone(),
        remote: cfg.remote.clone(),
        client_id: cfg.client_id.clone(),
        agent_path: cfg.agent_path.clone(),
        build_root: cfg.build_root.clone(),
        backend: cfg.backend.clone(),
        bazelisk_path: cfg.bazelisk_path.clone(),
        cargo_path: cfg.cargo_path.clone(),
        remote_path_entries: cfg.remote_path_entries.clone(),
    });

    let requested_transport = cli.transport.as_deref().unwrap_or(&selected.transport);
    if requested_transport == "ssh" && !workers.is_empty() {
        let start = workers
            .iter()
            .position(|w| w.name == selected.name)
            .unwrap_or(0);
        match choose_reachable_worker(&workers, start, |worker| {
            if worker.transport == "local" {
                return Ok(());
            }
            let destination = worker
                .remote
                .as_deref()
                .ok_or_else(|| "missing remote destination".to_string())?;
            probe_ssh(destination).map_err(|e| format!("{e:#}"))?;
            Ok(())
        }) {
            Ok((index, ())) => selected = workers[index].clone(),
            Err(failures) => anyhow::bail!("No reachable worker: {}", failures.join("; ")),
        }
    }

    let transport_mode = cli.transport.clone().unwrap_or(selected.transport);
    let remote = cli.remote.or(selected.remote);
    let agent_path = cli.agent_path.unwrap_or(selected.agent_path);
    let build_root = cli.build_root.unwrap_or(selected.build_root);

    let client_id = cli
        .client_id
        .or(selected.client_id)
        .or(cfg.client_id)
        .unwrap_or_else(|| {
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

    let project = project_config()?;
    let resolved_deps = resolve_and_validate_deps(&current_dir, &repo_name, &project.deps)
        .map_err(anyhow::Error::msg)?;

    let worktree_name = cli.worktree.unwrap_or_else(|| "main".to_string());
    let backend = select_backend(
        &current_dir,
        cli.backend.as_deref().or(selected.backend.as_deref()),
    )?;

    let path_entries = if cli.remote_path_entry.is_empty() {
        selected.remote_path_entries.clone()
    } else {
        cli.remote_path_entry.clone()
    };
    let artifact_selection = ArtifactSelection::parse(cli.artifacts.as_deref().unwrap_or("final"))
        .ok_or_else(|| anyhow::anyhow!("Unknown --artifacts value; expected final or all"))?;
    let remote_options = RemoteOptions {
        bazelisk_path: cli.bazelisk_path.unwrap_or(selected.bazelisk_path),
        cargo_path: cli.cargo_path.unwrap_or(selected.cargo_path),
        path_envs: path_prepend_envs(&path_entries),
        artifact_selection,
    };

    println!(
        "rbuild: Connecting via transport '{}' (client='{}', repo='{}', worktree='{}')",
        transport_mode, client_id, repo_name, worktree_name
    );

    let params = SessionWorkflowParams {
        local_exe_for_update: None,
        current_dir: &current_dir,
        client_id: &client_id,
        repo_name: &repo_name,
        worktree_name: &worktree_name,
        resolved_deps: &resolved_deps,
        excludes: &cfg.excludes,
        backend,
        artifact_globs: &cfg.artifact_globs,
        bazel_args: &cli.bazel_args,
        remote: &remote_options,
    };

    match transport_mode.as_str() {
        "local" => {
            let current_exe = std::env::current_exe().context("Locating current executable")?;
            let transport =
                LocalProcessTransport::spawn(&current_exe, Some(Path::new(&build_root)))?;
            let session = ClientSession::new(transport);
            let mut local_params = params;
            local_params.local_exe_for_update = Some(&current_exe);
            run_session_workflow(session, &local_params)
        }
        "ssh" => {
            let dest = remote.ok_or_else(|| {
                anyhow::anyhow!(
                    "SSH transport requires a remote destination. Specify via --remote or 'remote' in config"
                )
            })?;
            let transport = SshTransport::spawn(&dest, &agent_path, Some(&build_root))?;
            let mut session = ClientSession::new(transport);
            if let Err(error) = session.handshake(None) {
                if error
                    .to_string()
                    .contains("Cannot self-update cross-platform agent")
                {
                    build_and_install_native_agent(
                        &dest,
                        &agent_path,
                        Path::new(env!("CARGO_MANIFEST_DIR")),
                    )
                    .context("Building and installing native worker agent")?;
                } else {
                    return Err(error);
                }
            }
            drop(session);
            let transport = SshTransport::spawn(&dest, &agent_path, Some(&build_root))?;
            let session = ClientSession::new(transport);
            // In cross-platform environments (e.g. macOS client -> Linux agent), local binary must not be uploaded
            run_session_workflow(session, &params)
        }
        other => {
            anyhow::bail!("Unsupported transport '{}'. Use 'local' or 'ssh'", other);
        }
    }
}

/// Remote tool locations and PATH handling shared by build and cquery commands.
struct RemoteOptions {
    bazelisk_path: String,
    cargo_path: String,
    path_envs: Vec<(String, String)>,
    artifact_selection: ArtifactSelection,
}

#[derive(Clone)]
struct SessionWorkflowParams<'a> {
    local_exe_for_update: Option<&'a Path>,
    current_dir: &'a Path,
    client_id: &'a str,
    repo_name: &'a str,
    worktree_name: &'a str,
    resolved_deps: &'a [ResolvedDep],
    excludes: &'a [String],
    backend: BackendKind,
    artifact_globs: &'a [String],
    bazel_args: &'a [String],
    remote: &'a RemoteOptions,
}

fn run_session_workflow<T: Transport>(
    mut session: ClientSession<T>,
    params: &SessionWorkflowParams,
) -> Result<()> {
    let job_artifacts: Vec<String> = std::env::var("RBUILD_JOB_ARTIFACTS")
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default();
    // Handshake
    session.handshake(params.local_exe_for_update)?;
    println!("rbuild: Handshake successful.");

    // Sync dependencies first (if any)
    let mut dep_remote_paths = Vec::new();
    for dep in params.resolved_deps {
        let dep_remote_path =
            session.lock_worktree(params.client_id, &dep.name, params.worktree_name)?;
        println!(
            "rbuild: Dependency '{}' worktree locked at '{}'",
            dep.name, dep_remote_path
        );
        let dep_cache_path = params
            .current_dir
            .join(".rbuild")
            .join("deps")
            .join(format!("{}_manifest_cache.json", dep.name));
        if let Some(parent) = dep_cache_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut dep_cache = ManifestCache::load_from_file(&dep_cache_path);
        println!("rbuild: Synchronizing dependency '{}'...", dep.name);
        let dep_stats = session.sync_workspace(&dep.local_path, &mut dep_cache, params.excludes)?;
        let _ = dep_cache.save_to_file(&dep_cache_path);
        println!(
            "rbuild: Dependency '{}' sync complete (checked {}, transferred {}, deleted {})",
            dep.name, dep_stats.files_checked, dep_stats.files_transferred, dep_stats.files_deleted
        );
        dep_remote_paths.push((dep, dep_remote_path));
    }

    // Lock Worktree (Main repository locked last so agent's current_worktree is main)
    let remote_path =
        session.lock_worktree(params.client_id, params.repo_name, params.worktree_name)?;
    println!("rbuild: Worktree locked at '{}'", remote_path);

    // Sync (Main repository)
    let cache_path = params.current_dir.join(".rbuild/manifest_cache.json");
    let mut cache = ManifestCache::load_from_file(&cache_path);

    println!("rbuild: Synchronizing workspace...");
    let stats = session.sync_workspace(params.current_dir, &mut cache, params.excludes)?;
    let _ = cache.save_to_file(&cache_path);

    println!(
        "rbuild: Sync complete (checked {}, transferred {}, deleted {})",
        stats.files_checked, stats.files_transferred, stats.files_deleted
    );

    // If bazel args provided, run on agent (NEVER locally)
    if !params.bazel_args.is_empty() {
        let dep_path_pairs: Vec<(&str, &str)> = dep_remote_paths
            .iter()
            .map(|(dep, path)| (dep.name.as_str(), path.as_str()))
            .collect();
        let command_envs = build_dep_command_envs(&params.remote.path_envs, &dep_path_pairs);

        if params.backend == BackendKind::Cargo {
            let mut args = if params.bazel_args.first().is_some_and(|s| s == "build") {
                params.bazel_args[1..].to_vec()
            } else {
                params.bazel_args.to_vec()
            };
            args.retain(|a| !a.starts_with("--message-format="));
            args.splice(
                0..0,
                ["build".to_string(), "--message-format=json".to_string()],
            );
            // Cargo's JSON stream is buffered: only its human-readable rendering reaches stdout.
            progress::emit(&Progress::phase("build", "start"));
            let mut stdout = Vec::new();
            let code = session.run_remote_command_to_writers(
                &params.remote.cargo_path,
                &args,
                &command_envs,
                &mut stdout,
                &mut std::io::stderr(),
            )?;
            std::io::stdout().write_all(render_human_output(&stdout).as_bytes())?;
            std::io::stdout().flush()?;
            if code != 0 {
                std::process::exit(code);
            }
            let paths = artifact_paths(
                &stdout,
                Path::new(&remote_path),
                params.remote.artifact_selection,
            );
            println!("rbuild: Cargo reported {} artifact path(s).", paths.len());
            if !paths.is_empty() {
                let output = params.current_dir.join(".rbuild-out");
                session.collect_paths(&output, &paths)?;
                finalize_debug_artifacts(&output, &remote_path, params.current_dir)?;
            }
            if !job_artifacts.is_empty() {
                session.collect_paths(&params.current_dir.join(".rbuild-out"), &job_artifacts)?;
            }
            return Ok(());
        }
        if params.backend == BackendKind::Generic {
            let (program, args) = params
                .bazel_args
                .split_first()
                .ok_or_else(|| anyhow::anyhow!("Generic backend requires a command"))?;
            progress::emit(&Progress::phase("build", "start"));
            let code = session.run_remote_command(program, args, &command_envs)?;
            if code != 0 {
                std::process::exit(code);
            }
            if !params.artifact_globs.is_empty() {
                let out = params.current_dir.join(".rbuild-out");
                session.collect_paths(&out, params.artifact_globs)?;
                finalize_debug_artifacts(&out, &remote_path, params.current_dir)?;
            }
            if !job_artifacts.is_empty() {
                session.collect_paths(&params.current_dir.join(".rbuild-out"), &job_artifacts)?;
            }
            return Ok(());
        }

        let bazel_module_overrides: Vec<(&str, &str)> = dep_remote_paths
            .iter()
            .filter_map(|(dep, path)| {
                dep.bazel_module
                    .as_deref()
                    .map(|module| (module, path.as_str()))
            })
            .collect();
        let effective_bazel_args =
            inject_bazel_override_modules(params.bazel_args, &bazel_module_overrides);
        let parsed = parse_bazel_args(&effective_bazel_args);

        println!(
            "rbuild: Executing remote bazelisk command: {:?}",
            effective_bazel_args
        );
        progress::emit(&Progress::phase("build", "start"));
        let exit_code = session.run_remote_command(
            &params.remote.bazelisk_path,
            &effective_bazel_args,
            &command_envs,
        )?;
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
                let out_dir = params.current_dir.join(".rbuild-out");
                println!(
                    "rbuild: Collecting build artifacts into '{}'...",
                    out_dir.display()
                );
                let count = session.collect_artifacts_with_envs(
                    &out_dir,
                    Some(&params.remote.bazelisk_path),
                    &parsed.targets,
                    &parsed.flags,
                    &command_envs,
                )?;
                println!("rbuild: Successfully collected {} artifact(s).", count);
                finalize_debug_artifacts(&out_dir, &remote_path, params.current_dir)?;
            }
        }
        if !job_artifacts.is_empty() {
            session.collect_paths(&params.current_dir.join(".rbuild-out"), &job_artifacts)?;
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
    fn test_cli_parsing_remote_tool_flags() {
        let cli = Cli::try_parse_from([
            "rbuild",
            "--bazelisk-path",
            "/opt/bazel/bin/bazelisk",
            "--cargo-path",
            "/home/u/.cargo/bin/cargo",
            "--remote-path-entry",
            "/opt/a/bin",
            "--remote-path-entry",
            "/opt/b/bin",
            "--artifacts",
            "all",
            "build",
            "//pkg:target",
        ])
        .unwrap();
        assert_eq!(
            cli.bazelisk_path.as_deref(),
            Some("/opt/bazel/bin/bazelisk")
        );
        assert_eq!(cli.cargo_path.as_deref(), Some("/home/u/.cargo/bin/cargo"));
        assert_eq!(cli.remote_path_entry, vec!["/opt/a/bin", "/opt/b/bin"]);
        assert_eq!(cli.artifacts.as_deref(), Some("all"));
        assert_eq!(cli.bazel_args, vec!["build", "//pkg:target"]);

        assert!(Cli::try_parse_from(["rbuild", "--artifacts", "rlib"]).is_err());
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
    fn test_cli_parsing_named_worker() {
        let cli = Cli::try_parse_from([
            "rbuild",
            "--worker",
            "linux",
            "build",
            "--config=rp2350",
            "//firmware:target",
        ])
        .unwrap();
        assert_eq!(cli.worker.as_deref(), Some("linux"));
        assert_eq!(
            cli.bazel_args,
            vec!["build", "--config=rp2350", "//firmware:target"]
        );
    }

    #[test]
    fn test_parse_bazel_args_preserves_injected_override_modules() {
        let raw_args = vec![
            "build".to_string(),
            "--config=rp2350".to_string(),
            "//firmware:target".to_string(),
        ];
        let overrides = [
            ("shizuku", "/srv/build/user/shizuku/main"),
            ("pico_sdk", "/srv/build/user/pico_sdk/main"),
        ];
        let injected = inject_bazel_override_modules(&raw_args, &overrides);
        let parsed = parse_bazel_args(&injected);

        assert_eq!(parsed.subcommand.as_deref(), Some("build"));
        assert_eq!(parsed.targets, vec!["//firmware:target"]);
        assert_eq!(
            parsed.flags,
            vec![
                "--override_module=shizuku=/srv/build/user/shizuku/main",
                "--override_module=pico_sdk=/srv/build/user/pico_sdk/main",
                "--config=rp2350",
            ]
        );
        assert!(!parsed.is_ambiguous);
    }
}
