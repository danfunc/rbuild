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

/// Adds `--override_module=<module>=<worker_path>` flags to Bazel arguments.
///
/// Places the override flags immediately after the build/test/run subcommand
/// (and therefore before targets), matching Bazel command option syntax.
pub fn inject_bazel_override_modules(
    args: &[String],
    module_overrides: &[(&str, &str)],
) -> Vec<String> {
    if module_overrides.is_empty() {
        return args.to_vec();
    }
    let override_flags: Vec<String> = module_overrides
        .iter()
        .map(|(module, path)| format!("--override_module={module}={path}"))
        .collect();

    let subcmd_idx = args.iter().position(|arg| !arg.starts_with('-'));
    let mut result = Vec::with_capacity(args.len() + override_flags.len());

    match subcmd_idx {
        Some(idx) => {
            result.extend_from_slice(&args[..=idx]);
            result.extend(override_flags);
            result.extend_from_slice(&args[idx + 1..]);
        }
        None => {
            result.extend_from_slice(args);
            result.extend(override_flags);
        }
    }
    result
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

    #[test]
    fn test_inject_bazel_override_modules_position_and_targets() {
        // Simple build command
        let args = vec!["build".to_string(), "//pkg:app".to_string()];
        let overrides = [("shizuku", "/srv/build/c/shizuku/main")];
        let result = inject_bazel_override_modules(&args, &overrides);

        assert_eq!(
            result,
            vec![
                "build",
                "--override_module=shizuku=/srv/build/c/shizuku/main",
                "//pkg:app"
            ]
        );
        // Verify positions explicitly
        let subcmd_pos = result.iter().position(|x| x == "build").unwrap();
        let flag_pos = result
            .iter()
            .position(|x| x == "--override_module=shizuku=/srv/build/c/shizuku/main")
            .unwrap();
        let target_pos = result.iter().position(|x| x == "//pkg:app").unwrap();
        assert_eq!(subcmd_pos, 0);
        assert_eq!(flag_pos, 1);
        assert_eq!(target_pos, 2);
        assert!(flag_pos > subcmd_pos);
        assert!(flag_pos < target_pos);

        // Build with existing options
        let args_opts = vec![
            "build".to_string(),
            "--config=rp2350".to_string(),
            "-c".to_string(),
            "opt".to_string(),
            "//firmware:target".to_string(),
        ];
        let result_opts = inject_bazel_override_modules(&args_opts, &overrides);
        assert_eq!(
            result_opts,
            vec![
                "build",
                "--override_module=shizuku=/srv/build/c/shizuku/main",
                "--config=rp2350",
                "-c",
                "opt",
                "//firmware:target"
            ]
        );
        let opt_subcmd_pos = result_opts.iter().position(|x| x == "build").unwrap();
        let opt_flag_pos = result_opts
            .iter()
            .position(|x| x == "--override_module=shizuku=/srv/build/c/shizuku/main")
            .unwrap();
        let opt_target_pos = result_opts
            .iter()
            .position(|x| x == "//firmware:target")
            .unwrap();
        assert!(opt_flag_pos > opt_subcmd_pos);
        assert!(opt_flag_pos < opt_target_pos);

        // Startup flags before subcommand (e.g. test)
        let args_startup = vec![
            "--bazelrc=/dev/null".to_string(),
            "test".to_string(),
            "//test:all".to_string(),
        ];
        let result_startup = inject_bazel_override_modules(&args_startup, &overrides);
        assert_eq!(
            result_startup,
            vec![
                "--bazelrc=/dev/null",
                "test",
                "--override_module=shizuku=/srv/build/c/shizuku/main",
                "//test:all"
            ]
        );
        let st_subcmd_pos = result_startup.iter().position(|x| x == "test").unwrap();
        let st_flag_pos = result_startup
            .iter()
            .position(|x| x == "--override_module=shizuku=/srv/build/c/shizuku/main")
            .unwrap();
        let st_target_pos = result_startup
            .iter()
            .position(|x| x == "//test:all")
            .unwrap();
        assert_eq!(st_subcmd_pos, 1);
        assert_eq!(st_flag_pos, 2);
        assert_eq!(st_target_pos, 3);
        assert!(st_flag_pos > st_subcmd_pos);
        assert!(st_flag_pos < st_target_pos);

        // Multiple overrides
        let multiple_overrides = [
            ("shizuku", "/srv/build/c/shizuku/main"),
            ("pico_sdk", "/srv/build/c/pico_sdk/main"),
        ];
        let result_multi = inject_bazel_override_modules(&args, &multiple_overrides);
        assert_eq!(
            result_multi,
            vec![
                "build",
                "--override_module=shizuku=/srv/build/c/shizuku/main",
                "--override_module=pico_sdk=/srv/build/c/pico_sdk/main",
                "//pkg:app"
            ]
        );

        // Empty overrides: identity
        let result_empty = inject_bazel_override_modules(&args, &[]);
        assert_eq!(result_empty, args);
    }
}
