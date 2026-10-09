use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(default = "default_transport")]
    pub transport: String,

    pub remote: Option<String>,

    #[serde(default = "default_build_root")]
    pub build_root: String,

    pub client_id: Option<String>,

    #[serde(default)]
    pub extra_excludes: Vec<String>,
}

fn default_transport() -> String {
    "local".to_string()
}

fn default_build_root() -> String {
    "/srv/build".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            transport: default_transport(),
            remote: None,
            build_root: default_build_root(),
            client_id: None,
            extra_excludes: Vec::new(),
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

        if let Some(p) = path
            && p.exists()
            && let Ok(content) = fs::read_to_string(&p)
            && let Ok(cfg) = toml::from_str(&content)
        {
            return cfg;
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
build_root = "/var/build"
extra_excludes = ["*.tmp", "dist/"]
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.transport, "ssh");
        assert_eq!(cfg.remote.as_deref(), Some("build-box"));
        assert_eq!(cfg.build_root, "/var/build");
        assert_eq!(cfg.extra_excludes, vec!["*.tmp", "dist/"]);
    }
}
