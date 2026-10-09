//! Build backend selection and command planning.
//!
//! The backend is selected from explicit configuration first, then repository markers.

use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendKind {
    Bazel,
    Cargo,
    Generic,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bazel => "bazel",
            Self::Cargo => "cargo",
            Self::Generic => "generic",
        }
    }
}

pub fn select_backend(root: &Path, configured: Option<&str>) -> anyhow::Result<BackendKind> {
    if let Some(name) = configured {
        return parse_backend(name);
    }
    if root.join("MODULE.bazel").is_file()
        || root.join("WORKSPACE").is_file()
        || root.join("BUILD").is_file()
    {
        Ok(BackendKind::Bazel)
    } else if root.join("Cargo.toml").is_file() {
        Ok(BackendKind::Cargo)
    } else {
        Ok(BackendKind::Generic)
    }
}

fn parse_backend(name: &str) -> anyhow::Result<BackendKind> {
    match name {
        "bazel" => Ok(BackendKind::Bazel),
        "cargo" => Ok(BackendKind::Cargo),
        "generic" => Ok(BackendKind::Generic),
        _ => anyhow::bail!("Unknown build backend '{name}'; expected bazel, cargo, or generic"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn backend_auto_detects_markers_and_allows_override() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            select_backend(dir.path(), None).unwrap(),
            BackendKind::Generic
        );
        fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        assert_eq!(
            select_backend(dir.path(), None).unwrap(),
            BackendKind::Cargo
        );
        fs::write(dir.path().join("MODULE.bazel"), "module(name='x')\n").unwrap();
        assert_eq!(
            select_backend(dir.path(), None).unwrap(),
            BackendKind::Bazel
        );
        assert_eq!(
            select_backend(dir.path(), Some("cargo")).unwrap(),
            BackendKind::Cargo
        );
    }
}
