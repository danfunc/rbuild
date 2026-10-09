use serde::{Deserialize, Serialize};
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
        }
    }
}

impl Config {
    pub fn default_config_path() -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join(".config/rbuild.toml"))
    }

    pub fn load_or_default(explicit_path: Option<&Path>) -> Self {
        let path = explicit_path
            .map(PathBuf::from)
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
}
