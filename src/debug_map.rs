use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::path::Path;

/// Replaces `remote` with `local` only where `remote` ends at a path boundary.
///
/// A plain substring replace would also rewrite sibling trees such as `/remote/tree2`.
fn replace_root(text: &str, remote: &str, local: &str) -> String {
    if remote.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(idx) = rest.find(remote) {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + remote.len()..];
        let at_boundary = remote.ends_with('/')
            || after
                .chars()
                .next()
                .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.')));
        out.push_str(if at_boundary { local } else { remote });
        rest = after;
    }
    out.push_str(rest);
    out
}

fn replace_paths(value: &mut Value, remote: &str, local: &str) {
    match value {
        Value::String(s) => *s = replace_root(s, remote, local),
        Value::Array(items) => {
            for item in items {
                replace_paths(item, remote, local);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                replace_paths(item, remote, local);
            }
        }
        _ => {}
    }
}

pub fn rewrite_compile_commands(path: &Path, remote_root: &str, local_root: &str) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let bytes = fs::read(path).with_context(|| format!("Reading {}", path.display()))?;
    let mut value: Value =
        serde_json::from_slice(&bytes).context("Parsing compile_commands.json")?;
    replace_paths(&mut value, remote_root, local_root);
    fs::write(path, serde_json::to_vec_pretty(&value)?)
        .with_context(|| format!("Writing {}", path.display()))?;
    Ok(())
}

pub fn write_debug_map(out_dir: &Path, remote_root: &str, local_root: &str) -> Result<()> {
    fs::create_dir_all(out_dir)?;
    let mut source_map = serde_json::Map::new();
    source_map.insert(
        remote_root.to_string(),
        Value::String(local_root.to_string()),
    );
    let map = serde_json::json!({
        "remote_root": remote_root,
        "local_root": local_root,
        "gdb": format!("set substitute-path {} {}", remote_root, local_root),
        "vscode_cppdbg_sourceFileMap": source_map
    });
    fs::write(
        out_dir.join("rbuild-debug-map.json"),
        serde_json::to_vec_pretty(&map)?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compile_database_paths_are_rewritten_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("compile_commands.json");
        fs::write(&file, r#"[{"directory":"/remote/tree","file":"/remote/tree/a.cc","command":"cc -I/remote/tree/inc"}]"#).unwrap();
        rewrite_compile_commands(&file, "/remote/tree", "/local/tree").unwrap();
        let text = fs::read_to_string(file).unwrap();
        assert!(text.contains("/local/tree/a.cc"));
        assert!(text.contains("cc -I/local/tree/inc"));
        assert!(!text.contains("/remote/tree"));
    }

    #[test]
    fn sibling_trees_sharing_a_prefix_are_not_rewritten() {
        assert_eq!(
            replace_root(
                "/remote/tree2/a.cc /remote/tree/b.cc /remote/tree",
                "/remote/tree",
                "/local"
            ),
            "/remote/tree2/a.cc /local/b.cc /local"
        );
        assert_eq!(
            replace_root("-I/remote/tree.bak/inc", "/remote/tree", "/local"),
            "-I/remote/tree.bak/inc"
        );
    }

    #[test]
    fn debug_map_names_remote_and_local_roots() {
        let dir = tempfile::tempdir().unwrap();
        write_debug_map(dir.path(), "/remote/tree", "/local/tree").unwrap();
        let map: Value =
            serde_json::from_slice(&fs::read(dir.path().join("rbuild-debug-map.json")).unwrap())
                .unwrap();
        assert_eq!(map["gdb"], "set substitute-path /remote/tree /local/tree");
        assert_eq!(
            map["vscode_cppdbg_sourceFileMap"]["/remote/tree"],
            "/local/tree"
        );
    }
}
