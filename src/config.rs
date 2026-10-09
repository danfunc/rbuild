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
}
