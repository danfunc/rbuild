//! Parsing of `cargo build --message-format=json` output.
//!
//! Selects which produced files are retrieved and renders the human-readable part of the stream.

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

/// Which files of each `compiler-artifact` message are retrieved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactSelection {
    /// Executables and final library outputs (cdylib, staticlib, dylib). Build script outputs,
    /// rlib/rmeta/dep-info files, proc-macro outputs, and macOS dSYM bundles are skipped.
    Final,
    /// Every file reported by cargo, including build script outputs.
    All,
}

impl ArtifactSelection {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "final" => Some(Self::Final),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

/// Returns the human-readable text of a cargo JSON stream.
///
/// Only `compiler-message` renderings and a `build-finished` summary are emitted. Other JSON
/// objects are dropped; lines that are not JSON are kept verbatim.
pub fn render_human_output(json_lines: &[u8]) -> String {
    let mut out = String::new();
    for line in json_lines.split(|b| *b == b'\n') {
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            out.push_str(&String::from_utf8_lossy(line));
            out.push('\n');
            continue;
        };
        match value.get("reason").and_then(Value::as_str) {
            Some("compiler-message") => {
                if let Some(rendered) = value.pointer("/message/rendered").and_then(Value::as_str) {
                    out.push_str(rendered);
                    if !rendered.ends_with('\n') {
                        out.push('\n');
                    }
                }
            }
            Some("build-finished") => {
                let success = value
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                out.push_str(&format!(
                    "rbuild: cargo build finished (success={success})\n"
                ));
            }
            _ => {}
        }
    }
    out
}

/// Returns the worktree-relative paths of the files to retrieve, sorted and deduplicated.
pub fn artifact_paths(
    json_lines: &[u8],
    remote_root: &Path,
    selection: ArtifactSelection,
) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for line in json_lines.split(|b| *b == b'\n') {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if value.get("reason").and_then(Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let candidates = selected_files(&value, selection);
        for file in candidates {
            if let Some(p) = relativize_remote_path(Path::new(file), remote_root) {
                paths.insert(p);
            }
        }
    }
    paths.into_iter().collect()
}

fn selected_files(value: &Value, selection: ArtifactSelection) -> Vec<&str> {
    let is_build_script = value
        .pointer("/target/kind")
        .and_then(Value::as_array)
        .is_some_and(|kinds| kinds.iter().any(|k| k.as_str() == Some("custom-build")));
    let filenames: Vec<&str> = value
        .get("filenames")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let executable = value.get("executable").and_then(Value::as_str);

    match selection {
        ArtifactSelection::All => {
            let mut files = filenames;
            files.extend(executable);
            files
        }
        ArtifactSelection::Final => {
            if is_build_script {
                return Vec::new();
            }
            let crate_types: Vec<&str> = value
                .pointer("/target/crate_types")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            // proc-macro outputs are compile-time inputs of dependents, not deliverables, so they are excluded.
            let is_final_library = crate_types
                .iter()
                .any(|t| matches!(*t, "cdylib" | "staticlib" | "dylib"));
            if executable.is_none() && !is_final_library {
                return Vec::new();
            }
            let mut files: Vec<&str> = if is_final_library {
                filenames
                    .into_iter()
                    .filter(|f| !is_intermediate_file(f))
                    .collect()
            } else {
                Vec::new()
            };
            files.extend(executable);
            files
        }
    }
}

fn is_intermediate_file(file: &str) -> bool {
    file.ends_with(".dSYM")
        || file.ends_with(".d")
        || file.ends_with(".rlib")
        || file.ends_with(".rmeta")
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

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/srv/build/client/repo/main";

    fn stream(lines: &[&str]) -> Vec<u8> {
        lines.join("\n").into_bytes()
    }

    #[test]
    fn final_selection_keeps_executables_and_final_libraries_only() {
        let lines = [
            // Binary: executable plus dep-info, which is skipped.
            r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"crate_types":["bin"]},"filenames":["/srv/build/client/repo/main/target/release/rbuild","/srv/build/client/repo/main/target/release/rbuild.d"],"executable":"/srv/build/client/repo/main/target/release/rbuild"}"#,
            // Rlib: no executable, not a final library.
            r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"crate_types":["lib"]},"filenames":["/srv/build/client/repo/main/target/release/libdep.rlib","/srv/build/client/repo/main/target/release/libdep.rmeta"],"executable":null}"#,
            // cdylib: its files are kept, and the metadata files are dropped.
            r#"{"reason":"compiler-artifact","target":{"kind":["cdylib"],"crate_types":["cdylib"]},"filenames":["/srv/build/client/repo/main/target/release/libplug.so","/srv/build/client/repo/main/target/release/libplug.d"],"executable":null}"#,
            // Build script: excluded from the final selection.
            r#"{"reason":"compiler-artifact","target":{"kind":["custom-build"],"crate_types":["bin"]},"filenames":["/srv/build/client/repo/main/target/release/build/x/build-script-build"],"executable":"/srv/build/client/repo/main/target/release/build/x/build-script-build"}"#,
        ];
        let paths = artifact_paths(&stream(&lines), Path::new(ROOT), ArtifactSelection::Final);
        assert_eq!(
            paths,
            vec![
                "target/release/libplug.so".to_string(),
                "target/release/rbuild".to_string(),
            ]
        );
    }

    #[test]
    fn final_selection_includes_dylib_and_excludes_proc_macro() {
        let lines = [
            r#"{"reason":"compiler-artifact","target":{"kind":["dylib"],"crate_types":["dylib"]},"filenames":["/srv/build/client/repo/main/target/release/libshared.so","/srv/build/client/repo/main/target/release/libshared.d"],"executable":null}"#,
            r#"{"reason":"compiler-artifact","target":{"kind":["proc-macro"],"crate_types":["proc-macro"]},"filenames":["/srv/build/client/repo/main/target/release/libmacros.so","/srv/build/client/repo/main/target/release/libmacros.d"],"executable":null}"#,
            r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"crate_types":["rlib"]},"filenames":["/srv/build/client/repo/main/target/release/libdep.rlib","/srv/build/client/repo/main/target/release/libdep.rmeta"],"executable":null}"#,
        ];
        let final_paths =
            artifact_paths(&stream(&lines), Path::new(ROOT), ArtifactSelection::Final);
        assert_eq!(
            final_paths,
            vec!["target/release/libshared.so".to_string()],
            "dylib is final; proc-macro and rlib are intermediate"
        );

        let all_paths = artifact_paths(&stream(&lines), Path::new(ROOT), ArtifactSelection::All);
        assert!(all_paths.contains(&"target/release/libmacros.so".to_string()));
        assert!(all_paths.contains(&"target/release/libdep.rlib".to_string()));
        assert!(all_paths.contains(&"target/release/libshared.d".to_string()));
    }

    #[test]
    fn all_selection_keeps_every_reported_file() {
        let line = r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"crate_types":["lib"]},"filenames":["/srv/build/client/repo/main/target/release/libdep.rlib","/srv/build/client/repo/main/target/release/libdep.rmeta","/srv/build/client/repo/main/target/release/dep.d"],"executable":null}"#;
        let paths = artifact_paths(line.as_bytes(), Path::new(ROOT), ArtifactSelection::All);
        assert_eq!(
            paths,
            vec![
                "target/release/dep.d".to_string(),
                "target/release/libdep.rlib".to_string(),
                "target/release/libdep.rmeta".to_string(),
            ]
        );
    }

    #[test]
    fn all_selection_includes_build_script_outputs() {
        let line = r#"{"reason":"compiler-artifact","target":{"kind":["custom-build"],"crate_types":["bin"]},"filenames":["/srv/build/client/repo/main/target/release/build/x/build-script-build"],"executable":"/srv/build/client/repo/main/target/release/build/x/build-script-build"}"#;
        let final_paths =
            artifact_paths(line.as_bytes(), Path::new(ROOT), ArtifactSelection::Final);
        assert!(final_paths.is_empty());
        let all_paths = artifact_paths(line.as_bytes(), Path::new(ROOT), ArtifactSelection::All);
        assert_eq!(
            all_paths,
            vec!["target/release/build/x/build-script-build".to_string()]
        );
    }

    #[test]
    fn artifact_paths_are_rebased_across_path_aliases() {
        let line = r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"crate_types":["bin"]},"filenames":["/private/srv/build/client/repo/main/target/release/rbuild"],"executable":"/private/srv/build/client/repo/main/target/release/rbuild"}"#;
        let paths = artifact_paths(line.as_bytes(), Path::new(ROOT), ArtifactSelection::Final);
        assert_eq!(paths, vec!["target/release/rbuild".to_string()]);
    }

    #[test]
    fn human_output_shows_rendered_messages_and_summary_only() {
        let lines = [
            r#"{"reason":"compiler-artifact","target":{"kind":["bin"]},"filenames":["/x/rbuild"],"executable":"/x/rbuild"}"#,
            r#"{"reason":"compiler-message","message":{"rendered":"warning: unused variable: `x`\n"}}"#,
            r#"{"reason":"build-script-executed","package_id":"p"}"#,
            r#"{"reason":"build-finished","success":true}"#,
        ];
        let text = render_human_output(&stream(&lines));
        assert_eq!(
            text,
            "warning: unused variable: `x`\nrbuild: cargo build finished (success=true)\n"
        );
        assert!(!text.contains("reason"));
    }

    #[test]
    fn human_output_keeps_non_json_lines_and_reports_failure() {
        let lines = [
            "plain line",
            r#"{"reason":"build-finished","success":false}"#,
        ];
        let text = render_human_output(&stream(&lines));
        assert_eq!(
            text,
            "plain line\nrbuild: cargo build finished (success=false)\n"
        );
    }

    #[test]
    fn selection_parses_known_names_only() {
        assert_eq!(
            ArtifactSelection::parse("final"),
            Some(ArtifactSelection::Final)
        );
        assert_eq!(
            ArtifactSelection::parse("all"),
            Some(ArtifactSelection::All)
        );
        assert_eq!(ArtifactSelection::parse("rlib"), None);
    }
}
