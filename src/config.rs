use crate::sync::path_safety::is_safe_relative_path;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(default = "default_transport")]
    pub transport: String,

    pub remote: Option<String>,

    /// Build backend override: bazel, cargo, or generic. None auto-detects repository markers.
    pub backend: Option<String>,

    /// Generic backend glob patterns relative to the repository root.
    #[serde(default)]
    pub artifact_globs: Vec<String>,

    #[serde(default = "default_agent_path")]
    pub agent_path: String,

    #[serde(default = "default_build_root")]
    pub build_root: String,

    /// Remote bazelisk executable: a name resolved on the remote PATH or an absolute path.
    #[serde(default = "default_bazelisk_path")]
    pub bazelisk_path: String,

    /// Remote cargo executable: a name resolved on the remote PATH or an absolute path.
    #[serde(default = "default_cargo_path")]
    pub cargo_path: String,

    /// Directories prepended to the remote PATH of build and cquery commands. Empty by default.
    #[serde(default)]
    pub remote_path_entries: Vec<String>,

    pub client_id: Option<String>,

    #[serde(default, alias = "extra_excludes")]
    pub excludes: Vec<String>,

    #[serde(default)]
    pub workers: Vec<WorkerConfig>,
    #[serde(default)]
    pub default_worker: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectConfig {
    #[serde(default)]
    pub jobs: BTreeMap<String, JobConfig>,
    #[serde(default)]
    pub deps: BTreeMap<String, DepConfig>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DepConfig {
    pub path: String,
    #[serde(default)]
    pub bazel_module: Option<String>,
}

/// Transforms dependency name to environment variable name:
/// `RBUILD_DEP_<NAME>` (NAME is capitalized and non-alphanumeric chars become '_').
pub fn dep_env_var_name(dep_name: &str) -> String {
    let sanitized: String = dep_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("RBUILD_DEP_{}", sanitized)
}

/// Validates that a dependency name does not conflict with the main repository name,
/// reserved keywords, or contain unsafe path characters.
pub fn validate_dep_name(dep_name: &str, repo_name: &str) -> Result<(), String> {
    let trimmed = dep_name.trim();
    if trimmed.is_empty() {
        return Err("Dependency name cannot be empty".to_string());
    }
    if dep_name != trimmed {
        return Err(format!(
            "Dependency name '{}' contains leading or trailing whitespace",
            dep_name
        ));
    }
    if dep_name == repo_name {
        return Err(format!(
            "Dependency name '{}' conflicts with repository name '{}'",
            dep_name, repo_name
        ));
    }
    if matches!(
        dep_name,
        "local" | "." | ".." | ".git" | ".rbuild" | "agent"
    ) {
        return Err(format!("Dependency name '{}' is a reserved name", dep_name));
    }
    if dep_name.contains('/') || dep_name.contains('\\') || is_safe_relative_path(dep_name).is_err()
    {
        return Err(format!(
            "Dependency name '{}' is invalid: must be a safe single path component without separators",
            dep_name
        ));
    }
    Ok(())
}

/// Resolves dependency local path relative to `project_root` (or absolute),
/// verifying that it exists, is a directory, and is a git repository.
pub fn validate_and_resolve_dep_path(
    project_root: &Path,
    dep_name: &str,
    raw_path: &str,
) -> Result<PathBuf, String> {
    let path = Path::new(raw_path);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    if !resolved.exists() {
        return Err(format!(
            "Dependency '{}': path does not exist: '{}' (resolved to '{}')",
            dep_name,
            raw_path,
            resolved.display()
        ));
    }
    if !resolved.is_dir() {
        return Err(format!(
            "Dependency '{}': path is not a directory: '{}'",
            dep_name,
            resolved.display()
        ));
    }
    let git_check = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(&resolved)
        .output();
    match git_check {
        Ok(output) if output.status.success() => Ok(resolved),
        _ => Err(format!(
            "Dependency '{}': '{}' is not a git repository",
            dep_name,
            resolved.display()
        )),
    }
}

/// Computes the worker-side worktree directory path for a dependency or main repository.
pub fn worker_dep_path(
    build_root: &str,
    client_id: &str,
    dep_name: &str,
    worktree: &str,
) -> PathBuf {
    Path::new(build_root)
        .join(client_id)
        .join(dep_name)
        .join(worktree)
}

/// A validated dependency with resolved local path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedDep {
    pub name: String,
    pub local_path: PathBuf,
    pub bazel_module: Option<String>,
}

/// Resolves and validates all declared dependencies against `project_root` and `repo_name`.
pub fn resolve_and_validate_deps(
    project_root: &Path,
    repo_name: &str,
    deps: &BTreeMap<String, DepConfig>,
) -> Result<Vec<ResolvedDep>, String> {
    let mut resolved = Vec::with_capacity(deps.len());
    for (name, dep) in deps {
        validate_dep_name(name, repo_name)?;
        let local_path = validate_and_resolve_dep_path(project_root, name, &dep.path)?;
        resolved.push(ResolvedDep {
            name: name.clone(),
            local_path,
            bazel_module: dep.bazel_module.clone(),
        });
    }
    Ok(resolved)
}

/// Combines existing environment variables with `RBUILD_DEP_<NAME>=<path>` for dependencies.
pub fn build_dep_command_envs(
    path_envs: &[(String, String)],
    dep_remote_paths: &[(&str, &str)],
) -> Vec<(String, String)> {
    let mut envs = path_envs.to_vec();
    for (dep_name, dep_path) in dep_remote_paths {
        envs.push((dep_env_var_name(dep_name), dep_path.to_string()));
    }
    envs
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct JobConfig {
    pub worker: String,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub depends_on: Option<String>,
}

pub fn job_execution_order(project: &ProjectConfig, name: &str) -> Result<Vec<String>, String> {
    fn visit(
        project: &ProjectConfig,
        name: &str,
        visiting: &mut Vec<String>,
        done: &mut Vec<String>,
    ) -> Result<(), String> {
        if done.iter().any(|item| item == name) {
            return Ok(());
        }
        if visiting.iter().any(|item| item == name) {
            return Err(format!(
                "Job dependency cycle: {} -> {name}",
                visiting.join(" -> ")
            ));
        }
        let job = project
            .jobs
            .get(name)
            .ok_or_else(|| format!("Undefined job '{name}'"))?;
        visiting.push(name.to_string());
        if let Some(dep) = &job.depends_on {
            visit(project, dep, visiting, done)?;
        }
        visiting.pop();
        done.push(name.to_string());
        Ok(())
    }
    let mut done = Vec::new();
    visit(project, name, &mut Vec::new(), &mut done)?;
    Ok(done)
}

pub fn resolve_job_worker<'a>(
    job: &JobConfig,
    workers: &'a [WorkerConfig],
) -> Result<Option<&'a WorkerConfig>, String> {
    if job.worker == "local" {
        return Ok(None);
    }
    workers
        .iter()
        .find(|worker| worker.name == job.worker)
        .map(Some)
        .ok_or_else(|| format!("Undefined worker '{}'", job.worker))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct WorkerConfig {
    pub name: String,
    #[serde(default = "default_transport")]
    pub transport: String,
    pub remote: Option<String>,
    pub client_id: Option<String>,
    #[serde(default = "default_agent_path")]
    pub agent_path: String,
    #[serde(default = "default_build_root")]
    pub build_root: String,
    pub backend: Option<String>,
    #[serde(default = "default_bazelisk_path")]
    pub bazelisk_path: String,
    #[serde(default = "default_cargo_path")]
    pub cargo_path: String,
    #[serde(default)]
    pub remote_path_entries: Vec<String>,
}

fn default_transport() -> String {
    "local".to_string()
}

fn default_agent_path() -> String {
    "rbuild".to_string()
}

fn default_build_root() -> String {
    "/srv/build".to_string()
}

fn default_bazelisk_path() -> String {
    "bazelisk".to_string()
}

fn default_cargo_path() -> String {
    "cargo".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            transport: default_transport(),
            remote: None,
            backend: None,
            artifact_globs: Vec::new(),
            agent_path: default_agent_path(),
            build_root: default_build_root(),
            bazelisk_path: default_bazelisk_path(),
            cargo_path: default_cargo_path(),
            remote_path_entries: Vec::new(),
            client_id: None,
            excludes: Vec::new(),
            workers: Vec::new(),
            default_worker: None,
        }
    }
}

impl Config {
    pub fn resolved_workers(&self) -> Vec<WorkerConfig> {
        if !self.workers.is_empty() {
            return self.workers.clone();
        }
        self.remote
            .as_ref()
            .map(|_| WorkerConfig {
                name: "default".into(),
                transport: self.transport.clone(),
                remote: self.remote.clone(),
                client_id: self.client_id.clone(),
                agent_path: self.agent_path.clone(),
                build_root: self.build_root.clone(),
                backend: self.backend.clone(),
                bazelisk_path: self.bazelisk_path.clone(),
                cargo_path: self.cargo_path.clone(),
                remote_path_entries: self.remote_path_entries.clone(),
            })
            .into_iter()
            .collect()
    }

    pub fn default_config_path() -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join(".config/rbuild.toml"))
    }

    pub fn load_or_default(explicit_path: Option<&Path>) -> Self {
        let path = explicit_path
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("RBUILD_CONFIG").map(PathBuf::from))
            .or_else(Self::default_config_path);

        if let Some(p) = path {
            if p.exists() {
                if let Ok(content) = fs::read_to_string(&p) {
                    if let Ok(cfg) = toml::from_str(&content) {
                        return cfg;
                    }
                }
            }
        }

        Self::default()
    }
}

impl ProjectConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| format!("Cannot read project config {}: {e}", path.display()))?;
        toml::from_str(&contents)
            .map_err(|e| format!("Invalid project config {}: {e}", path.display()))
    }
}

/// Attempts the requested/default worker first, then remaining workers in config order.
/// Only the supplied connection probe controls failover; callers must probe before building.
pub fn choose_reachable_worker<T>(
    workers: &[WorkerConfig],
    start: usize,
    mut probe: impl FnMut(&WorkerConfig) -> Result<T, String>,
) -> Result<(usize, T), Vec<String>> {
    let mut errors = Vec::new();
    for offset in 0..workers.len() {
        let index = (start + offset) % workers.len();
        match probe(&workers[index]) {
            Ok(value) => return Ok((index, value)),
            Err(error) => errors.push(format!("{}: {error}", workers[index].name)),
        }
    }
    Err(errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_parsing() {
        let toml_str = r#"
transport = "ssh"
remote = "build-box"
agent_path = "/usr/local/bin/rbuild"
build_root = "/var/build"
client_id = "test-client"
backend = "cargo"
artifact_globs = ["target/release/rbuild"]
excludes = ["*.tmp", "dist/"]
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.transport, "ssh");
        assert_eq!(cfg.remote.as_deref(), Some("build-box"));
        assert_eq!(cfg.agent_path, "/usr/local/bin/rbuild");
        assert_eq!(cfg.build_root, "/var/build");
        assert_eq!(cfg.client_id.as_deref(), Some("test-client"));
        assert_eq!(cfg.backend.as_deref(), Some("cargo"));
        assert_eq!(cfg.artifact_globs, vec!["target/release/rbuild"]);
        assert_eq!(cfg.excludes, vec!["*.tmp", "dist/"]);
    }

    #[test]
    fn test_config_backward_compatible_extra_excludes_and_defaults() {
        let toml_str = r#"
remote = "remote-host"
extra_excludes = ["legacy/"]
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.transport, "local");
        assert_eq!(cfg.remote.as_deref(), Some("remote-host"));
        assert_eq!(cfg.agent_path, "rbuild");
        assert_eq!(cfg.build_root, "/srv/build");
        assert_eq!(cfg.client_id, None);
        assert_eq!(cfg.excludes, vec!["legacy/"]);
    }

    #[test]
    fn test_remote_tool_settings_default_and_parse() {
        let defaults: Config = toml::from_str("remote = \"h\"\n").unwrap();
        assert_eq!(defaults.bazelisk_path, "bazelisk");
        assert_eq!(defaults.cargo_path, "cargo");
        assert!(defaults.remote_path_entries.is_empty());

        let toml_str = r#"
bazelisk_path = "/opt/bazel/bin/bazelisk"
cargo_path = "/home/u/.cargo/bin/cargo"
remote_path_entries = ["/opt/bazel/bin", "/usr/local/go/bin"]
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.bazelisk_path, "/opt/bazel/bin/bazelisk");
        assert_eq!(cfg.cargo_path, "/home/u/.cargo/bin/cargo");
        assert_eq!(
            cfg.remote_path_entries,
            vec!["/opt/bazel/bin", "/usr/local/go/bin"]
        );
    }

    #[test]
    fn legacy_single_remote_resolves_as_one_named_worker() {
        let cfg: Config =
            toml::from_str("transport='ssh'\nremote='host'\nagent_path='/bin/rbuild'\n").unwrap();
        let workers = cfg.resolved_workers();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].name, "default");
        assert_eq!(workers[0].remote.as_deref(), Some("host"));
        assert_eq!(workers[0].agent_path, "/bin/rbuild");
        assert_eq!(workers[0].client_id, None);
    }

    #[test]
    fn named_workers_resolve_and_keep_config_order() {
        let cfg: Config = toml::from_str("default_worker='linux'\n[[workers]]\nname='mac'\nremote='mac'\n[[workers]]\nname='linux'\nremote='linux'\nbackend='bazel'\n").unwrap();
        let workers = cfg.resolved_workers();
        assert_eq!(
            workers.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
            vec!["mac", "linux"]
        );
        assert_eq!(workers[1].backend.as_deref(), Some("bazel"));
    }

    #[test]
    fn named_worker_client_id_is_parsed() {
        let cfg: Config = toml::from_str(
            "client_id='top-level'\n[[workers]]\nname='mac'\nremote='mac'\nclient_id='macbook'\n",
        )
        .unwrap();
        assert_eq!(cfg.client_id.as_deref(), Some("top-level"));
        assert_eq!(
            cfg.resolved_workers()[0].client_id.as_deref(),
            Some("macbook")
        );
    }

    #[test]
    fn project_and_user_worker_settings_compose_and_jobs_resolve() {
        let user: Config =
            toml::from_str("[[workers]]\nname='main-lxc'\nremote='builder'\n").unwrap();
        let project: ProjectConfig = toml::from_str("[jobs.build]\nworker='main-lxc'\nargs=['build','//x:y']\n[jobs.run]\nworker='local'\ncommand=['flash','out.uf2']\ndepends_on='build'\n").unwrap();
        let workers = user.resolved_workers();
        assert_eq!(
            resolve_job_worker(&project.jobs["build"], &workers)
                .unwrap()
                .unwrap()
                .name,
            "main-lxc"
        );
        assert!(
            resolve_job_worker(&project.jobs["run"], &workers)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            job_execution_order(&project, "run").unwrap(),
            vec!["build", "run"]
        );
        let bad: ProjectConfig = toml::from_str("[jobs.build]\nworker='missing'\n").unwrap();
        assert!(
            resolve_job_worker(&bad.jobs["build"], &workers)
                .unwrap_err()
                .contains("Undefined worker 'missing'")
        );
    }

    #[test]
    fn job_without_dep_runs_alone_and_missing_dependency_errors() {
        let project: ProjectConfig =
            toml::from_str("[jobs.run]\nworker='local'\ncommand=['true']\n").unwrap();
        assert_eq!(job_execution_order(&project, "run").unwrap(), vec!["run"]);
        let broken: ProjectConfig =
            toml::from_str("[jobs.run]\nworker='local'\ncommand=['true']\ndepends_on='build'\n")
                .unwrap();
        assert!(
            job_execution_order(&broken, "run")
                .unwrap_err()
                .contains("Undefined job 'build'")
        );
    }

    #[test]
    fn shizuku_project_example_is_valid_and_runs_build_before_local_run() {
        let project: ProjectConfig =
            toml::from_str(include_str!("../examples/shizuku.rbuild.toml")).unwrap();
        assert_eq!(project.jobs["build"].worker, "main-lxc");
        assert_eq!(project.jobs["run"].worker, "local");
        assert_eq!(
            job_execution_order(&project, "run").unwrap(),
            vec!["build", "uf2", "run"]
        );
    }

    #[test]
    fn connection_failure_selects_next_worker_and_reports_all_failures() {
        let cfg: Config = toml::from_str("[[workers]]\nname='a'\nremote='a'\n[[workers]]\nname='b'\nremote='b'\n[[workers]]\nname='c'\nremote='c'\n").unwrap();
        let workers = cfg.resolved_workers();
        let (index, chosen) = choose_reachable_worker(&workers, 0, |w| {
            if w.name == "a" {
                Err("offline".into())
            } else {
                Ok(w.name.clone())
            }
        })
        .unwrap();
        assert_eq!(index, 1);
        assert_eq!(chosen, "b");
        let failures =
            choose_reachable_worker::<()>(&workers, 0, |_| Err("unreachable".into())).unwrap_err();
        assert_eq!(
            failures,
            vec!["a: unreachable", "b: unreachable", "c: unreachable"]
        );
    }

    #[test]
    fn explicit_worker_selection_starts_at_that_worker_then_wraps_in_config_order() {
        let cfg: Config = toml::from_str(
            "[[workers]]\nname='a'\nremote='a'\n[[workers]]\nname='b'\nremote='b'\n[[workers]]\nname='c'\nremote='c'\n",
        )
        .unwrap();
        let workers = cfg.resolved_workers();
        let mut attempted = Vec::new();
        let (index, _) = choose_reachable_worker(&workers, 1, |w| {
            attempted.push(w.name.clone());
            if w.name == "c" {
                Ok(())
            } else {
                Err("offline".into())
            }
        })
        .unwrap();
        assert_eq!(attempted, vec!["b", "c"]);
        assert_eq!(index, 2);
    }

    #[test]
    fn build_failure_is_not_a_connection_failover_signal() {
        let cfg: Config = toml::from_str(
            "[[workers]]\nname='a'\nremote='a'\n[[workers]]\nname='b'\nremote='b'\n",
        )
        .unwrap();
        let workers = cfg.resolved_workers();
        let mut connection_attempts = 0;
        let chosen = choose_reachable_worker(&workers, 0, |_| {
            connection_attempts += 1;
            Ok("connected")
        })
        .expect("connection succeeds");
        assert_eq!(chosen.0, 0);
        assert_eq!(connection_attempts, 1);
        let build: Result<(), &str> = Err("remote build exited 1");
        assert!(matches!(build, Err("remote build exited 1")));
        assert_eq!(
            connection_attempts, 1,
            "build failures do not probe/fail over to another worker"
        );
    }

    #[test]
    fn test_deps_toml_parsing_relative_absolute_and_bazel_module() {
        let toml_str = r#"
[deps.shizuku]
path = "../Shizuku-v3"
bazel_module = "shizuku"

[deps.external_lib]
path = "/opt/libs/ext"
"#;
        let project: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(project.deps.len(), 2);

        let shizuku = &project.deps["shizuku"];
        assert_eq!(shizuku.path, "../Shizuku-v3");
        assert_eq!(shizuku.bazel_module.as_deref(), Some("shizuku"));

        let ext = &project.deps["external_lib"];
        assert_eq!(ext.path, "/opt/libs/ext");
        assert_eq!(ext.bazel_module, None);
    }

    #[test]
    fn test_deps_empty_backward_compatibility() {
        let toml_str = r#"
[jobs.build]
worker = "main-lxc"
args = ["build", "//pkg:app"]
"#;
        let project: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert!(project.deps.is_empty());
        assert_eq!(project.jobs.len(), 1);
        assert_eq!(project.jobs["build"].worker, "main-lxc");
    }

    #[test]
    fn test_dep_env_var_name_transformation() {
        assert_eq!(dep_env_var_name("shizuku"), "RBUILD_DEP_SHIZUKU");
        assert_eq!(dep_env_var_name("Shizuku-v3"), "RBUILD_DEP_SHIZUKU_V3");
        assert_eq!(
            dep_env_var_name("my.cool-lib_2"),
            "RBUILD_DEP_MY_COOL_LIB_2"
        );
        assert_eq!(dep_env_var_name("abc 123"), "RBUILD_DEP_ABC_123");
    }

    #[test]
    fn test_worker_dep_path_construction() {
        let path = worker_dep_path("/srv/build", "macbook", "shizuku", "main");
        assert_eq!(path, PathBuf::from("/srv/build/macbook/shizuku/main"));

        let main_path = worker_dep_path("/srv/build", "macbook", "my-app", "main");
        assert_ne!(path, main_path);
        assert_eq!(main_path, PathBuf::from("/srv/build/macbook/my-app/main"));
    }

    #[test]
    fn test_validate_dep_name_conflicts_and_reserved() {
        // Main repository name conflict
        let err = validate_dep_name("my-repo", "my-repo").unwrap_err();
        assert_eq!(
            err,
            "Dependency name 'my-repo' conflicts with repository name 'my-repo'"
        );

        // Reserved names
        assert_eq!(
            validate_dep_name("local", "my-repo").unwrap_err(),
            "Dependency name 'local' is a reserved name"
        );
        assert_eq!(
            validate_dep_name("agent", "my-repo").unwrap_err(),
            "Dependency name 'agent' is a reserved name"
        );
        assert_eq!(
            validate_dep_name(".", "my-repo").unwrap_err(),
            "Dependency name '.' is a reserved name"
        );
        assert_eq!(
            validate_dep_name("..", "my-repo").unwrap_err(),
            "Dependency name '..' is a reserved name"
        );
        assert_eq!(
            validate_dep_name(".git", "my-repo").unwrap_err(),
            "Dependency name '.git' is a reserved name"
        );
        assert_eq!(
            validate_dep_name(".rbuild", "my-repo").unwrap_err(),
            "Dependency name '.rbuild' is a reserved name"
        );

        // Path separators / unsafe characters
        assert!(
            validate_dep_name("foo/bar", "my-repo")
                .unwrap_err()
                .contains("must be a safe single path component without separators")
        );
        assert!(
            validate_dep_name("foo\\bar", "my-repo")
                .unwrap_err()
                .contains("must be a safe single path component without separators")
        );

        // Empty or whitespace
        assert_eq!(
            validate_dep_name("", "my-repo").unwrap_err(),
            "Dependency name cannot be empty"
        );
        assert_eq!(
            validate_dep_name("   ", "my-repo").unwrap_err(),
            "Dependency name cannot be empty"
        );
        assert_eq!(
            validate_dep_name(" foo ", "my-repo").unwrap_err(),
            "Dependency name ' foo ' contains leading or trailing whitespace"
        );

        // Valid name
        assert!(validate_dep_name("shizuku", "my-repo").is_ok());
        assert!(validate_dep_name("pico-sdk_v2", "my-repo").is_ok());
    }

    #[test]
    fn test_validate_and_resolve_dep_path() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path().join("project");
        let valid_dep = temp_dir.path().join("dep_repo");
        let non_git_dir = temp_dir.path().join("plain_dir");
        fs::create_dir_all(&project_root).unwrap();
        fs::create_dir_all(&valid_dep).unwrap();
        fs::create_dir_all(&non_git_dir).unwrap();

        // Initialize git in valid_dep
        let status = std::process::Command::new("git")
            .args(["init"])
            .current_dir(&valid_dep)
            .status()
            .unwrap();
        assert!(status.success());

        // Relative path resolution to valid git repo
        let resolved =
            validate_and_resolve_dep_path(&project_root, "my_dep", "../dep_repo").unwrap();
        assert_eq!(
            resolved.canonicalize().unwrap(),
            valid_dep.canonicalize().unwrap()
        );

        // Absolute path resolution
        let resolved_abs =
            validate_and_resolve_dep_path(&project_root, "my_dep", valid_dep.to_str().unwrap())
                .unwrap();
        assert_eq!(
            resolved_abs.canonicalize().unwrap(),
            valid_dep.canonicalize().unwrap()
        );

        // Missing path
        let missing_err =
            validate_and_resolve_dep_path(&project_root, "missing", "../non_existent").unwrap_err();
        assert!(missing_err.contains("path does not exist"));

        // Directory is not a git repository
        let non_git_err =
            validate_and_resolve_dep_path(&project_root, "plain", "../plain_dir").unwrap_err();
        assert!(non_git_err.contains("is not a git repository"));
    }

    #[test]
    fn test_resolve_and_validate_deps() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path().join("my-main-repo");
        let dep1_dir = temp_dir.path().join("dep1");
        fs::create_dir_all(&project_root).unwrap();
        fs::create_dir_all(&dep1_dir).unwrap();

        let status = std::process::Command::new("git")
            .args(["init"])
            .current_dir(&dep1_dir)
            .status()
            .unwrap();
        assert!(status.success());

        let mut deps = BTreeMap::new();
        deps.insert(
            "shizuku".to_string(),
            DepConfig {
                path: "../dep1".to_string(),
                bazel_module: Some("shizuku_mod".to_string()),
            },
        );

        let resolved = resolve_and_validate_deps(&project_root, "my-main-repo", &deps).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "shizuku");
        assert_eq!(resolved[0].bazel_module.as_deref(), Some("shizuku_mod"));
        assert_eq!(
            resolved[0].local_path.canonicalize().unwrap(),
            dep1_dir.canonicalize().unwrap()
        );

        // Empty deps succeeds with empty vector
        let empty_deps = BTreeMap::new();
        let resolved_empty =
            resolve_and_validate_deps(&project_root, "my-main-repo", &empty_deps).unwrap();
        assert!(resolved_empty.is_empty());

        // Conflicting dep name with main repo
        let mut conflict_deps = BTreeMap::new();
        conflict_deps.insert(
            "my-main-repo".to_string(),
            DepConfig {
                path: "../dep1".to_string(),
                bazel_module: None,
            },
        );
        let conflict_err =
            resolve_and_validate_deps(&project_root, "my-main-repo", &conflict_deps).unwrap_err();
        assert!(conflict_err.contains("conflicts with repository name"));
    }

    #[test]
    fn test_build_dep_command_envs() {
        let base_envs = vec![("PATH".to_string(), "/custom/bin".to_string())];
        let dep_paths = [
            ("shizuku", "/srv/build/c/shizuku/main"),
            ("foo-bar.baz_1", "/srv/build/c/foo-bar.baz_1/main"),
        ];
        let result = build_dep_command_envs(&base_envs, &dep_paths);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], ("PATH".to_string(), "/custom/bin".to_string()));
        assert_eq!(
            result[1],
            (
                "RBUILD_DEP_SHIZUKU".to_string(),
                "/srv/build/c/shizuku/main".to_string()
            )
        );
        assert_eq!(
            result[2],
            (
                "RBUILD_DEP_FOO_BAR_BAZ_1".to_string(),
                "/srv/build/c/foo-bar.baz_1/main".to_string()
            )
        );

        // Empty deps leaves base envs unchanged
        let empty_result = build_dep_command_envs(&base_envs, &[]);
        assert_eq!(empty_result, base_envs);
    }
}
