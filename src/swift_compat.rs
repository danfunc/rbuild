use crate::swift_manifest::{
    BinOp, BuildCondition, CompilerSetting, FuncParam, InterfaceMethod, InterfaceProperty,
    LinkerSetting, PackageDependency, PackageManifest, ParseError, ProductKind, Span, StructField,
    SwiftExpr, SwiftStmt, TargetDependency, TargetKind, VersionRequirement, parse_swift_ast,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Converts a [`PackageManifest`] into the exact JSON schema produced by
/// `swift package dump-package` (Swift 6.0 `PackageDescription` serialization format).
pub fn to_swift_dump_package_json(
    manifest: &PackageManifest,
    package_root: Option<&Path>,
) -> Value {
    let root_str = package_root
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());

    let c_std = manifest
        .c_language_standard
        .as_deref()
        .map(normalize_dump_c_std);
    let cxx_std = manifest
        .cxx_language_standard
        .as_deref()
        .map(normalize_dump_cxx_std);

    let dependencies: Vec<Value> = manifest
        .dependencies
        .iter()
        .map(|dep| match dep {
            PackageDependency::Local { name, path } => {
                let identity = name.clone().unwrap_or_else(|| infer_package_identity(path));
                let resolved_path = if Path::new(path).is_absolute() || package_root.is_none() {
                    path.clone()
                } else if let Some(root) = package_root {
                    let joined = root.join(path);
                    std::fs::canonicalize(&joined)
                        .unwrap_or(joined)
                        .to_string_lossy()
                        .to_string()
                } else {
                    path.clone()
                };
                json!({
                    "fileSystem": [
                        {
                            "identity": identity,
                            "path": resolved_path,
                            "productFilter": null,
                            "traits": [
                                { "name": "default" }
                            ]
                        }
                    ]
                })
            }
            PackageDependency::Git {
                name,
                url,
                requirement,
            } => {
                let identity = name.clone().unwrap_or_else(|| infer_package_identity(url));
                let req_json = match requirement {
                    VersionRequirement::From(ver) | VersionRequirement::UpToNextMajor(ver) => {
                        let upper = next_major_version(ver);
                        json!({
                            "range": [
                                {
                                    "lowerBound": ver,
                                    "upperBound": upper
                                }
                            ]
                        })
                    }
                    VersionRequirement::UpToNextMinor(ver) => {
                        let upper = next_minor_version(ver);
                        json!({
                            "range": [
                                {
                                    "lowerBound": ver,
                                    "upperBound": upper
                                }
                            ]
                        })
                    }
                    VersionRequirement::Exact(ver) => json!({
                        "exact": [ver]
                    }),
                    VersionRequirement::Branch(b) => json!({
                        "branch": [b]
                    }),
                    VersionRequirement::Revision(r) => json!({
                        "revision": [r]
                    }),
                };
                json!({
                    "sourceControl": [
                        {
                            "identity": identity,
                            "location": {
                                "remote": [
                                    { "urlString": url }
                                ]
                            },
                            "productFilter": null,
                            "requirement": req_json,
                            "traits": [
                                { "name": "default" }
                            ]
                        }
                    ]
                })
            }
        })
        .collect();

    let platforms: Vec<Value> = manifest
        .platforms
        .iter()
        .map(|p| {
            let ver = p
                .version_or_board
                .as_deref()
                .map(normalize_platform_version)
                .unwrap_or_else(|| "1.0".to_string());
            json!({
                "options": [],
                "platformName": p.name.to_ascii_lowercase(),
                "version": ver
            })
        })
        .collect();

    let products: Vec<Value> = manifest
        .products
        .iter()
        .map(|prod| {
            let type_json = match &prod.kind {
                ProductKind::Library { linkage } => {
                    let mode = linkage.as_deref().unwrap_or("automatic");
                    json!({ "library": [mode] })
                }
                ProductKind::Executable => json!({ "executable": null }),
                ProductKind::Firmware { format } => json!({
                    "executable": null,
                    "firmwareFormat": format
                }),
                ProductKind::Plugin => json!({ "plugin": null }),
            };
            json!({
                "name": prod.name,
                "settings": [],
                "targets": prod.targets,
                "type": type_json
            })
        })
        .collect();

    let targets: Vec<Value> = manifest
        .targets
        .iter()
        .map(|t| {
            let deps_json: Vec<Value> = t
                .dependencies
                .iter()
                .map(|d| match d {
                    TargetDependency::ByName { name, condition } => json!({
                        "byName": [
                            name,
                            condition_to_swift_json(condition.as_ref())
                        ]
                    }),
                    TargetDependency::Target { name, condition } => json!({
                        "target": [
                            name,
                            condition_to_swift_json(condition.as_ref())
                        ]
                    }),
                    TargetDependency::Product {
                        name,
                        package,
                        condition,
                    } => json!({
                        "product": [
                            name,
                            package,
                            null,
                            condition_to_swift_json(condition.as_ref())
                        ]
                    }),
                })
                .collect();

            let mut settings_json: Vec<Value> = Vec::new();
            for cs in &t.c_settings {
                if let Some(v) = compiler_setting_to_swift_json(cs, "c") {
                    settings_json.push(v);
                }
            }
            for cxxs in &t.cxx_settings {
                if let Some(v) = compiler_setting_to_swift_json(cxxs, "cxx") {
                    settings_json.push(v);
                }
            }
            for sws in &t.swift_settings {
                if let Some(v) = compiler_setting_to_swift_json(sws, "swift") {
                    settings_json.push(v);
                }
            }
            for ls in &t.linker_settings {
                if let Some(v) = linker_setting_to_swift_json(ls) {
                    settings_json.push(v);
                }
            }

            let target_type = match t.kind {
                TargetKind::Regular => "regular",
                TargetKind::Executable => "executable",
                TargetKind::Test => "test",
                TargetKind::System => "system",
                TargetKind::Binary => "binary",
            };

            let mut obj = serde_json::Map::new();
            obj.insert("dependencies".to_string(), json!(deps_json));
            obj.insert("exclude".to_string(), json!(t.exclude));
            obj.insert("name".to_string(), json!(t.name));
            obj.insert("packageAccess".to_string(), json!(true));
            if let Some(p) = &t.path {
                obj.insert("path".to_string(), json!(p));
            }
            if let Some(pub_hdr) = &t.public_headers_path {
                obj.insert("publicHeadersPath".to_string(), json!(pub_hdr));
            }
            obj.insert("resources".to_string(), json!([]));
            obj.insert("settings".to_string(), json!(settings_json));
            if !t.sources.is_empty() {
                obj.insert("sources".to_string(), json!(t.sources));
            }
            obj.insert("type".to_string(), json!(target_type));
            Value::Object(obj)
        })
        .collect();

    let tools_ver = manifest
        .tools_version
        .as_deref()
        .map(normalize_semver_three)
        .unwrap_or_else(|| "6.0.0".to_string());

    json!({
        "cLanguageStandard": c_std,
        "cxxLanguageStandard": cxx_std,
        "defaultLocalization": manifest.default_localization,
        "dependencies": dependencies,
        "name": manifest.name,
        "packageKind": {
            "root": [root_str]
        },
        "pkgConfig": null,
        "platforms": platforms,
        "products": products,
        "providers": null,
        "swiftLanguageVersions": null,
        "targets": targets,
        "toolsVersion": {
            "_version": tools_ver
        },
        "traits": []
    })
}

fn normalize_dump_c_std(s: &str) -> String {
    s.replace('_', "")
}

fn normalize_dump_cxx_std(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("cxx") {
        format!("c++{rest}")
    } else if let Some(rest) = s.strip_prefix("gnu_cxx") {
        format!("gnu++{rest}")
    } else {
        s.to_string()
    }
}

fn infer_package_identity(path_or_url: &str) -> String {
    let trimmed = path_or_url.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    last.strip_suffix(".git")
        .unwrap_or(last)
        .to_ascii_lowercase()
}

fn next_major_version(ver: &str) -> String {
    let mut parts = ver.split('.');
    let major: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    format!("{}.0.0", major + 1)
}

fn next_minor_version(ver: &str) -> String {
    let mut parts = ver.split('.');
    let major: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    format!("{}.{}.0", major, minor + 1)
}

fn normalize_platform_version(raw: &str) -> String {
    let stripped = raw.strip_prefix('v').unwrap_or(raw).replace('_', ".");
    if stripped.contains('.') {
        stripped
    } else {
        format!("{stripped}.0")
    }
}

fn normalize_semver_three(raw: &str) -> String {
    let count = raw.split('.').count();
    match count {
        1 => format!("{raw}.0.0"),
        2 => format!("{raw}.0"),
        _ => raw.to_string(),
    }
}

fn condition_to_swift_json(cond: Option<&BuildCondition>) -> Value {
    let Some(c) = cond else {
        return Value::Null;
    };
    let mut map = serde_json::Map::new();
    if let Some(cfg) = &c.configuration {
        map.insert("config".to_string(), json!(cfg.to_ascii_lowercase()));
    }
    if !c.platforms.is_empty() {
        let lower: Vec<String> = c
            .platforms
            .iter()
            .map(|p| p.to_ascii_lowercase())
            .collect();
        map.insert("platformNames".to_string(), json!(lower));
    }
    if map.is_empty() {
        Value::Null
    } else {
        Value::Object(map)
    }
}

fn compiler_setting_to_swift_json(setting: &CompilerSetting, tool: &str) -> Option<Value> {
    let (kind, cond) = match setting {
        CompilerSetting::Define {
            name,
            value,
            condition,
        } => {
            let combined = match value {
                Some(v) => format!("{name}={v}"),
                None => name.clone(),
            };
            (json!({ "define": { "_0": combined } }), condition.as_ref())
        }
        CompilerSetting::HeaderSearchPath { path, condition } => (
            json!({ "headerSearchPath": { "_0": path } }),
            condition.as_ref(),
        ),
        CompilerSetting::UnsafeFlags { flags, condition } => (
            json!({ "unsafeFlags": { "_0": flags } }),
            condition.as_ref(),
        ),
        CompilerSetting::Standard { .. } => return None,
    };

    let mut map = serde_json::Map::new();
    let cond_val = condition_to_swift_json(cond);
    if !cond_val.is_null() {
        map.insert("condition".to_string(), cond_val);
    }
    map.insert("kind".to_string(), kind);
    map.insert("tool".to_string(), json!(tool));
    Some(Value::Object(map))
}

fn linker_setting_to_swift_json(setting: &LinkerSetting) -> Option<Value> {
    let (kind, cond) = match setting {
        LinkerSetting::LinkedLibrary { library, condition } => (
            json!({ "linkedLibrary": { "_0": library } }),
            condition.as_ref(),
        ),
        LinkerSetting::LinkedFramework {
            framework,
            condition,
        } => (
            json!({ "linkedFramework": { "_0": framework } }),
            condition.as_ref(),
        ),
        LinkerSetting::UnsafeFlags { flags, condition } => (
            json!({ "unsafeFlags": { "_0": flags } }),
            condition.as_ref(),
        ),
        LinkerSetting::GcSections { condition } => (
            json!({ "unsafeFlags": { "_0": ["-Wl,--gc-sections"] } }),
            condition.as_ref(),
        ),
        LinkerSetting::LinkerScript { path, condition } => (
            json!({ "unsafeFlags": { "_0": [format!("-T{path}")] } }),
            condition.as_ref(),
        ),
        LinkerSetting::OutputFormat { .. } => return None,
    };

    let mut map = serde_json::Map::new();
    let cond_val = condition_to_swift_json(cond);
    if !cond_val.is_null() {
        map.insert("condition".to_string(), cond_val);
    }
    map.insert("kind".to_string(), kind);
    map.insert("tool".to_string(), json!("linker"));
    Some(Value::Object(map))
}

// ============================================================================
// Swift Compiler AST JSON Serializer (`swift-frontend -dump-ast -dump-ast-format json`)
// ============================================================================

/// Emits a JSON AST matching the schema of `swift-frontend -dump-ast -dump-ast-format json`.
pub fn emit_swift_ast_json(source: &str, filename: &str) -> Result<Value, ParseError> {
    let stmts = parse_swift_ast(source)?;
    let line_offsets = compute_line_offsets(source);

    let items: Vec<Value> = stmts
        .iter()
        .map(|stmt| stmt_to_swift_ast_json(stmt, &line_offsets))
        .collect();

    Ok(json!({
        "_kind": "source_file",
        "filename": filename,
        "compiler_version": {
            "_kind": "compiler_version",
            "major": "6",
            "minor": "4",
            "full": "rbuild Swift/Ada Frontend 0.3.0 (compatible with Apple Swift 6.4)"
        },
        "items": items
    }))
}

fn compute_line_offsets(source: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (idx, b) in source.bytes().enumerate() {
        if b == b'\n' {
            offsets.push(idx + 1);
        }
    }
    offsets
}

fn span_to_offset(span: Span, line_offsets: &[usize]) -> usize {
    let line_idx = span.line.saturating_sub(1);
    let base = line_offsets.get(line_idx).copied().unwrap_or(0);
    base + span.column.saturating_sub(1)
}

fn swift_mangled_type(ty: Option<&str>) -> String {
    match ty.unwrap_or("Any") {
        "String" => "$sSSD".to_string(),
        "Int" | "Int64" => "$sSiD".to_string(),
        "Bool" => "$sSbD".to_string(),
        "Double" => "$sSdD".to_string(),
        "Float" => "$sSfD".to_string(),
        "Void" | "()" => "$sytD".to_string(),
        other if other.starts_with('[') && other.ends_with(']') => {
            let inner = &other[1..other.len() - 1];
            format!("$sSay{}GD", swift_mangled_type(Some(inner)).trim_start_matches("$s").trim_end_matches('D'))
        }
        other => format!("$s{}{other}VD", other.len()),
    }
}

fn stmt_to_swift_ast_json(stmt: &SwiftStmt, line_offsets: &[usize]) -> Value {
    match stmt {
        SwiftStmt::Import { module, span } => {
            let start = span_to_offset(*span, line_offsets);
            json!({
                "_kind": "import_decl",
                "range": { "start": start, "end": start + 6 + module.len() },
                "module_path": [module]
            })
        }
        SwiftStmt::InterfaceDecl {
            name,
            properties,
            methods,
            span,
        } => {
            let start = span_to_offset(*span, line_offsets);
            let mut members = Vec::new();
            for prop in properties {
                members.push(interface_prop_to_ast_json(prop, line_offsets));
            }
            for m in methods {
                members.push(interface_method_to_ast_json(m, line_offsets));
            }
            json!({
                "_kind": "protocol",
                "range": { "start": start, "end": start },
                "name": {
                    "_kind": "decl_name",
                    "base_name": {
                        "_kind": "base_name",
                        "name": name
                    }
                },
                "access": "internal",
                "members": members
            })
        }
        SwiftStmt::StructDecl {
            name,
            conformances,
            fields,
            methods,
            span,
        } => {
            let start = span_to_offset(*span, line_offsets);
            let conf_json: Vec<Value> = conformances
                .iter()
                .map(|c| {
                    json!({
                        "_kind": "normal_conformance",
                        "type": swift_mangled_type(Some(name)),
                        "protocol": c,
                        "source_kind": "explicit"
                    })
                })
                .collect();

            let mut members = Vec::new();
            for field in fields {
                members.extend(struct_field_to_ast_json(field, line_offsets));
            }
            for m in methods {
                members.push(stmt_to_swift_ast_json(m, line_offsets));
            }

            json!({
                "_kind": "struct_decl",
                "range": { "start": start, "end": start },
                "name": {
                    "_kind": "decl_name",
                    "base_name": {
                        "_kind": "base_name",
                        "name": name
                    }
                },
                "interface_type": swift_mangled_type(Some(name)),
                "access": "internal",
                "inherits": {
                    "_kind": "inheritance",
                    "conformances": conf_json
                },
                "members": members
            })
        }
        SwiftStmt::FuncDecl {
            name,
            params,
            return_type,
            body,
            span,
        } => func_to_ast_json(name, params, return_type.as_deref(), body, *span, line_offsets),
        SwiftStmt::Binding {
            name,
            mutable,
            type_annotation,
            value,
            span,
        } => {
            let start = span_to_offset(*span, line_offsets);
            let ty_str = swift_mangled_type(type_annotation.as_deref());
            json!({
                "_kind": "var_decl",
                "range": { "start": start, "end": start },
                "name": {
                    "_kind": "decl_name",
                    "base_name": {
                        "_kind": "base_name",
                        "name": name
                    }
                },
                "interface_type": ty_str,
                "immutable": !mutable,
                "init": expr_to_swift_ast_json(value, line_offsets)
            })
        }
        SwiftStmt::Return { value, span } => {
            let start = span_to_offset(*span, line_offsets);
            json!({
                "_kind": "return_stmt",
                "range": { "start": start, "end": start },
                "result": value.as_ref().map(|e| expr_to_swift_ast_json(e, line_offsets))
            })
        }
        SwiftStmt::Expr(expr) => expr_to_swift_ast_json(expr, line_offsets),
    }
}

fn interface_prop_to_ast_json(prop: &InterfaceProperty, line_offsets: &[usize]) -> Value {
    let start = span_to_offset(prop.span, line_offsets);
    json!({
        "_kind": "var_decl",
        "range": { "start": start, "end": start },
        "name": {
            "_kind": "decl_name",
            "base_name": {
                "_kind": "base_name",
                "name": prop.name
            }
        },
        "interface_type": swift_mangled_type(Some(&prop.ty)),
        "access": "internal",
        "readImpl": "getter",
        "immutable": true
    })
}

fn interface_method_to_ast_json(m: &InterfaceMethod, line_offsets: &[usize]) -> Value {
    func_to_ast_json(
        &m.name,
        &m.params,
        m.return_type.as_deref(),
        m.default_body.as_deref().unwrap_or(&[]),
        m.span,
        line_offsets,
    )
}

fn struct_field_to_ast_json(field: &StructField, line_offsets: &[usize]) -> Vec<Value> {
    let start = span_to_offset(field.span, line_offsets);
    let ty_mangled = swift_mangled_type(field.ty.as_deref());
    let init_json = field
        .default_value
        .as_ref()
        .map(|e| expr_to_swift_ast_json(e, line_offsets));

    vec![
        json!({
            "_kind": "pattern_binding_decl",
            "range": { "start": start, "end": start },
            "pattern_entries": [
                {
                    "_kind": "pattern_entry",
                    "pattern": {
                        "_kind": "pattern_typed",
                        "type": ty_mangled,
                        "sub_pattern": {
                            "_kind": "pattern_named",
                            "type": ty_mangled,
                            "name": {
                                "_kind": "decl_name",
                                "base_name": {
                                    "_kind": "base_name",
                                    "name": field.name
                                }
                            }
                        }
                    },
                    "original_init": init_json
                }
            ]
        }),
        json!({
            "_kind": "var_decl",
            "range": { "start": start, "end": start },
            "name": {
                "_kind": "decl_name",
                "base_name": {
                    "_kind": "base_name",
                    "name": field.name
                }
            },
            "interface_type": ty_mangled,
            "access": "internal",
            "immutable": !field.mutable
        }),
    ]
}

fn func_to_ast_json(
    name: &str,
    params: &[FuncParam],
    return_type: Option<&str>,
    body: &[SwiftStmt],
    span: Span,
    line_offsets: &[usize],
) -> Value {
    let start = span_to_offset(span, line_offsets);
    let arg_labels: Vec<String> = params
        .iter()
        .map(|p| p.external_label.clone().unwrap_or_default())
        .collect();
    let param_items: Vec<Value> = params
        .iter()
        .map(|p| {
            let mut obj = serde_json::Map::new();
            obj.insert("_kind".to_string(), json!("parameter"));
            obj.insert(
                "name".to_string(),
                json!({
                    "_kind": "decl_name",
                    "base_name": {
                        "_kind": "base_name",
                        "name": p.name
                    }
                }),
            );
            if let Some(ext) = &p.external_label {
                if ext != &p.name {
                    obj.insert("apiName".to_string(), json!(ext));
                }
            }
            obj.insert(
                "interface_type".to_string(),
                json!(swift_mangled_type(Some(&p.ty))),
            );
            Value::Object(obj)
        })
        .collect();

    let elements: Vec<Value> = body
        .iter()
        .map(|s| stmt_to_swift_ast_json(s, line_offsets))
        .collect();

    json!({
        "_kind": "func_decl",
        "range": { "start": start, "end": start },
        "name": {
            "_kind": "decl_name",
            "base_name": {
                "_kind": "base_name",
                "name": name
            },
            "args": arg_labels
        },
        "access": "internal",
        "result": swift_mangled_type(return_type),
        "params": {
            "_kind": "parameter_list",
            "params": param_items
        },
        "body": {
            "_kind": "brace_stmt",
            "elements": elements
        }
    })
}

fn expr_to_swift_ast_json(expr: &SwiftExpr, line_offsets: &[usize]) -> Value {
    let start = span_to_offset(expr.span(), line_offsets);
    match expr {
        SwiftExpr::String(s, _) => json!({
            "_kind": "string_literal_expr",
            "type": "$sSSD",
            "range": { "start": start, "end": start },
            "encoding": "utf8",
            "value": s
        }),
        SwiftExpr::Int(n, _) => json!({
            "_kind": "integer_literal_expr",
            "type": "$sSiD",
            "range": { "start": start, "end": start },
            "value": n.to_string()
        }),
        SwiftExpr::Float(f, _) => json!({
            "_kind": "float_literal_expr",
            "type": "$sSdD",
            "range": { "start": start, "end": start },
            "value": f.to_string()
        }),
        SwiftExpr::Bool(b, _) => json!({
            "_kind": "boolean_literal_expr",
            "type": "$sSbD",
            "range": { "start": start, "end": start },
            "value": b
        }),
        SwiftExpr::Nil(_) => json!({
            "_kind": "nil_literal_expr",
            "range": { "start": start, "end": start }
        }),
        SwiftExpr::Ident(name, _) => json!({
            "_kind": "declref_expr",
            "range": { "start": start, "end": start },
            "decl": {
                "_kind": "decl_ref",
                "base_name": name
            }
        }),
        SwiftExpr::ShorthandArg(idx, _) => json!({
            "_kind": "declref_expr",
            "range": { "start": start, "end": start },
            "decl": {
                "_kind": "decl_ref",
                "base_name": format!("${idx}")
            }
        }),
        SwiftExpr::Array(items, _) => {
            let elements: Vec<Value> = items
                .iter()
                .map(|e| expr_to_swift_ast_json(e, line_offsets))
                .collect();
            json!({
                "_kind": "array_expr",
                "range": { "start": start, "end": start },
                "elements": elements
            })
        }
        SwiftExpr::Member { base, name, .. } => json!({
            "_kind": "member_ref_expr",
            "range": { "start": start, "end": start },
            "base": base.as_ref().map(|b| expr_to_swift_ast_json(b, line_offsets)),
            "decl": {
                "_kind": "decl_ref",
                "base_name": name
            }
        }),
        SwiftExpr::Call { callee, args, .. } => {
            let arg_items: Vec<Value> = args
                .iter()
                .map(|a| {
                    let mut m = serde_json::Map::new();
                    m.insert("_kind".to_string(), json!("argument"));
                    if let Some(lbl) = &a.label {
                        m.insert("label".to_string(), json!(lbl));
                    }
                    m.insert(
                        "expr".to_string(),
                        expr_to_swift_ast_json(&a.value, line_offsets),
                    );
                    Value::Object(m)
                })
                .collect();
            json!({
                "_kind": "call_expr",
                "range": { "start": start, "end": start },
                "fn": expr_to_swift_ast_json(callee, line_offsets),
                "args": {
                    "_kind": "argument_list",
                    "args": arg_items
                }
            })
        }
        SwiftExpr::Closure { params, body, .. } => {
            let elements: Vec<Value> = body
                .iter()
                .map(|s| stmt_to_swift_ast_json(s, line_offsets))
                .collect();
            let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
            json!({
                "_kind": "closure_expr",
                "range": { "start": start, "end": start },
                "params": param_names,
                "body": {
                    "_kind": "brace_stmt",
                    "elements": elements
                }
            })
        }
        SwiftExpr::Binary {
            op, left, right, ..
        } => {
            let op_str = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Eq => "==",
                BinOp::NotEq => "!=",
                BinOp::Lt => "<",
                BinOp::Le => "<=",
                BinOp::Gt => ">",
                BinOp::Ge => ">=",
            };
            json!({
                "_kind": "binary_expr",
                "range": { "start": start, "end": start },
                "fn": {
                    "_kind": "declref_expr",
                    "decl": {
                        "_kind": "decl_ref",
                        "base_name": op_str
                    }
                },
                "args": {
                    "_kind": "argument_list",
                    "args": [
                        { "_kind": "argument", "expr": expr_to_swift_ast_json(left, line_offsets) },
                        { "_kind": "argument", "expr": expr_to_swift_ast_json(right, line_offsets) }
                    ]
                }
            })
        }
        SwiftExpr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            let then_els: Vec<Value> = then_branch
                .iter()
                .map(|s| stmt_to_swift_ast_json(s, line_offsets))
                .collect();
            let else_els: Vec<Value> = else_branch
                .iter()
                .map(|s| stmt_to_swift_ast_json(s, line_offsets))
                .collect();
            json!({
                "_kind": "if_expr",
                "range": { "start": start, "end": start },
                "cond": expr_to_swift_ast_json(cond, line_offsets),
                "then": { "_kind": "brace_stmt", "elements": then_els },
                "else": { "_kind": "brace_stmt", "elements": else_els }
            })
        }
    }
}

// ============================================================================
// Ada-Inspired Memory-Safety & Range-Constrained Type Verifier ("Easy-to-Write Ada")
// ============================================================================

/// Parameter passing & ownership mode (combining Ada's `in` / `in out` / `out` with
/// Swift 5.9+ / SPARK `borrowing` / `inout` / `consuming` non-aliasing rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamOwnershipMode {
    /// Immutable borrow (`in` in Ada, `borrowing` in Swift). Multiple shared borrows allowed,
    /// but cannot alias with an active `InOut` or `Consuming` parameter in the same call.
    BorrowingIn,
    /// Exclusive mutable borrow (`in out` in Ada, `inout` in Swift). Must pass a mutable `var`
    /// variable, and must not alias any other argument in the same call.
    InOut,
    /// Ownership transfer / move (`out` / move in Ada SPARK, `consuming` in Swift).
    /// Consumes the binding so it cannot be read or written afterwards unless re-initialized.
    ConsumingOut,
}

/// Ada-style range-constrained scalar subtype (`subtype ObjectId = Int in 0...63`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeSubtypeSpec {
    pub name: String,
    pub base_type: String,
    pub lower_bound: i64,
    pub upper_bound: i64,
    pub is_distinct: bool,
    pub span: Span,
}

/// Static memory-safety and Ada range-constraint verifier.
#[derive(Debug, Clone, Default)]
pub struct AdaSafetyChecker {
    pub range_subtypes: BTreeMap<String, RangeSubtypeSpec>,
    pub var_mutability: BTreeMap<String, bool>,
    pub var_types: BTreeMap<String, String>,
    pub consumed_vars: BTreeSet<String>,
    pub func_param_modes: BTreeMap<String, Vec<(String, ParamOwnershipMode, String)>>,
}

impl AdaSafetyChecker {
    pub fn register_range_subtype(&mut self, spec: RangeSubtypeSpec) {
        self.range_subtypes.insert(spec.name.clone(), spec);
    }

    pub fn register_func_signature(
        &mut self,
        func_name: &str,
        params: Vec<(String, ParamOwnershipMode, String)>,
    ) {
        self.func_param_modes.insert(func_name.to_string(), params);
    }

    /// Checks a variable binding (`let`/`var`) against Ada range subtypes and ownership state.
    pub fn check_binding(
        &mut self,
        name: &str,
        mutable: bool,
        type_annotation: Option<&str>,
        value: &SwiftExpr,
        span: Span,
    ) -> Result<(), ParseError> {
        self.check_expr_not_consumed(value)?;

        if let Some(ty_name) = type_annotation {
            if let Some(range_spec) = self.range_subtypes.get(ty_name) {
                if let Some(const_int) = eval_static_int(value) {
                    if const_int < range_spec.lower_bound || const_int > range_spec.upper_bound {
                        return Err(ParseError {
                            span,
                            message: format!(
                                "Ada range check failed: value {const_int} is out of range {}...{} for subtype '{}'",
                                range_spec.lower_bound, range_spec.upper_bound, range_spec.name
                            ),
                        });
                    }
                }
                // Strong distinct nominal typing check: reject assigning a different distinct type
                if range_spec.is_distinct {
                    if let SwiftExpr::Ident(src_var, var_span) = value {
                        if let Some(src_ty) = self.var_types.get(src_var) {
                            if src_ty != ty_name {
                                return Err(ParseError {
                                    span: *var_span,
                                    message: format!(
                                        "Ada distinct type mismatch: cannot implicitly convert '{}' (of type '{src_ty}') to distinct type '{ty_name}'",
                                        src_var
                                    ),
                                });
                            }
                        }
                    }
                }
            }
            self.var_types.insert(name.to_string(), ty_name.to_string());
        }

        self.var_mutability.insert(name.to_string(), mutable);
        self.consumed_vars.remove(name);
        Ok(())
    }

    /// Verifies a function call for:
    /// 1. Use-after-move (`consuming` / `out` parameter already consumed)
    /// 2. Mutability requirement (`inout` requires a `var`, not a `let`)
    /// 3. Non-aliasing law (Ada 2012 / SPARK / Rust borrow law: an `inout` argument cannot alias
    ///    any other `in`, `inout`, or `consuming` argument in the same call)
    /// 4. Static range bounds on arguments targeting range-constrained subtypes
    pub fn check_call(
        &mut self,
        func_name: &str,
        args: &[SwiftExpr],
        call_span: Span,
    ) -> Result<(), ParseError> {
        for arg in args {
            self.check_expr_not_consumed(arg)?;
        }

        let Some(param_specs) = self.func_param_modes.get(func_name).cloned() else {
            return Ok(());
        };

        let mut shared_borrows: BTreeSet<String> = BTreeSet::new();
        let mut exclusive_borrows: BTreeSet<String> = BTreeSet::new();

        for (arg_expr, (param_name, mode, param_ty)) in args.iter().zip(param_specs.iter()) {
            // Range subtype check on literal/constant arguments
            if let Some(range_spec) = self.range_subtypes.get(param_ty) {
                if let Some(val) = eval_static_int(arg_expr) {
                    if val < range_spec.lower_bound || val > range_spec.upper_bound {
                        return Err(ParseError {
                            span: arg_expr.span(),
                            message: format!(
                                "Ada range check failed for parameter '{param_name}' of '{func_name}': value {val} is outside {}...{} ({param_ty})",
                                range_spec.lower_bound, range_spec.upper_bound
                            ),
                        });
                    }
                }
            }

            if let SwiftExpr::Ident(var_name, var_span) = arg_expr {
                match mode {
                    ParamOwnershipMode::BorrowingIn => {
                        if exclusive_borrows.contains(var_name) {
                            return Err(ParseError {
                                span: *var_span,
                                message: format!(
                                    "memory safety violation in call to '{func_name}': cannot borrow '{var_name}' as immutable `in` while it is also borrowed as mutable `inout`"
                                ),
                            });
                        }
                        shared_borrows.insert(var_name.clone());
                    }
                    ParamOwnershipMode::InOut => {
                        let is_mut = self.var_mutability.get(var_name).copied().unwrap_or(false);
                        if !is_mut {
                            return Err(ParseError {
                                span: *var_span,
                                message: format!(
                                    "memory safety violation in call to '{func_name}': cannot pass immutable `let {var_name}` to `inout` parameter '{param_name}'"
                                ),
                            });
                        }
                        if exclusive_borrows.contains(var_name)
                            || shared_borrows.contains(var_name)
                        {
                            return Err(ParseError {
                                span: *var_span,
                                message: format!(
                                    "memory safety violation in call to '{func_name}': mutable `inout` parameter '{param_name}' aliases '{var_name}' which is already borrowed in the same call"
                                ),
                            });
                        }
                        exclusive_borrows.insert(var_name.clone());
                    }
                    ParamOwnershipMode::ConsumingOut => {
                        if exclusive_borrows.contains(var_name)
                            || shared_borrows.contains(var_name)
                        {
                            return Err(ParseError {
                                span: *var_span,
                                message: format!(
                                    "memory safety violation in call to '{func_name}': `consuming` parameter '{param_name}' aliases '{var_name}'"
                                ),
                            });
                        }
                        exclusive_borrows.insert(var_name.clone());
                        self.consumed_vars.insert(var_name.clone());
                    }
                }
            } else if *mode == ParamOwnershipMode::InOut {
                return Err(ParseError {
                    span: call_span,
                    message: format!(
                        "memory safety violation in call to '{func_name}': `inout` parameter '{param_name}' requires a mutable variable binding"
                    ),
                });
            }
        }

        Ok(())
    }

    pub fn check_expr_not_consumed(&self, expr: &SwiftExpr) -> Result<(), ParseError> {
        match expr {
            SwiftExpr::Ident(name, span) => {
                if self.consumed_vars.contains(name) {
                    return Err(ParseError {
                        span: *span,
                        message: format!(
                            "memory safety violation: use of moved/consumed value '{name}'"
                        ),
                    });
                }
                Ok(())
            }
            SwiftExpr::Array(items, _) => {
                for item in items {
                    self.check_expr_not_consumed(item)?;
                }
                Ok(())
            }
            SwiftExpr::Call { callee, args, .. } => {
                self.check_expr_not_consumed(callee)?;
                for a in args {
                    self.check_expr_not_consumed(&a.value)?;
                }
                Ok(())
            }
            SwiftExpr::Binary { left, right, .. } => {
                self.check_expr_not_consumed(left)?;
                self.check_expr_not_consumed(right)
            }
            _ => Ok(()),
        }
    }
}

fn eval_static_int(expr: &SwiftExpr) -> Option<i64> {
    match expr {
        SwiftExpr::Int(n, _) => Some(*n),
        SwiftExpr::Binary { op, left, right, .. } => {
            let l = eval_static_int(left)?;
            let r = eval_static_int(right)?;
            match op {
                BinOp::Add => Some(l + r),
                BinOp::Sub => Some(l - r),
                BinOp::Mul => Some(l * r),
                BinOp::Div if r != 0 => Some(l / r),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swift_manifest::parse_package_swift;

    #[test]
    fn test_swift_dump_package_json_matches_apple_swiftpm_schema() {
        let src = r#"// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "Shizuku",
    defaultLocalization: "en",
    platforms: [.macOS(.v14), .iOS("17.2")],
    products: [
        .library(name: "kernel_static", type: .static, targets: ["kernel"]),
        .library(name: "kernel_auto", targets: ["kernel"]),
        .executable(name: "shizuku", targets: ["shizuku"]),
    ],
    dependencies: [
        .package(path: "/tmp/pico-sdk"),
        .package(url: "https://example.com/foo-bar.git", from: "1.2.3"),
        .package(url: "https://example.com/exact.git", exact: "2.0.0"),
        .package(url: "https://example.com/branch.git", branch: "main"),
    ],
    targets: [
        .target(
            name: "kernel",
            path: "source/kernel",
            exclude: ["old.cpp"],
            sources: ["init.cpp"],
            publicHeadersPath: "include",
            dependencies: [
                .product(name: "PicoStdlib", package: "pico-sdk", condition: .when(platforms: [.macOS])),
            ],
            cSettings: [
                .define("C_DEF"),
                .unsafeFlags(["-Wall"]),
            ],
            cxxSettings: [
                .define("PICO_RP2350", to: "1", .when(platforms: [.linux], configuration: .debug)),
                .headerSearchPath("internal_headers"),
            ],
            linkerSettings: [
                .linkedLibrary("m"),
                .linkedFramework("CoreFoundation", .when(platforms: [.macOS])),
                .unsafeFlags(["-Wl,--gc-sections"]),
            ]
        ),
        .executableTarget(
            name: "shizuku",
            path: "firmware",
            sources: ["main.cpp"],
            dependencies: [
                .target(name: "kernel", condition: .when(platforms: [.linux])),
            ]
        ),
        .testTarget(
            name: "kernelTests",
            dependencies: ["kernel"]
        ),
    ],
    cLanguageStandard: .c17,
    cxxLanguageStandard: .cxx20
)
"#;
        let pkg = parse_package_swift(src).unwrap();
        let dumped = to_swift_dump_package_json(&pkg, Some(Path::new("/workspace/Shizuku")));

        assert_eq!(dumped["name"], "Shizuku");
        assert_eq!(dumped["cLanguageStandard"], "c17");
        assert_eq!(dumped["cxxLanguageStandard"], "c++20");
        assert_eq!(dumped["defaultLocalization"], "en");
        assert_eq!(dumped["toolsVersion"]["_version"], "6.0.0");
        assert_eq!(dumped["packageKind"]["root"][0], "/workspace/Shizuku");

        // Dependencies match SwiftPM's fileSystem / sourceControl structure
        assert_eq!(
            dumped["dependencies"][0]["fileSystem"][0]["identity"],
            "pico-sdk"
        );
        assert_eq!(
            dumped["dependencies"][1]["sourceControl"][0]["requirement"]["range"][0]["lowerBound"],
            "1.2.3"
        );
        assert_eq!(
            dumped["dependencies"][1]["sourceControl"][0]["requirement"]["range"][0]["upperBound"],
            "2.0.0"
        );
        assert_eq!(
            dumped["dependencies"][2]["sourceControl"][0]["requirement"]["exact"][0],
            "2.0.0"
        );
        assert_eq!(
            dumped["dependencies"][3]["sourceControl"][0]["requirement"]["branch"][0],
            "main"
        );

        // Products match SwiftPM's library: ["static"] / ["automatic"] and executable: null
        assert_eq!(dumped["products"][0]["type"]["library"][0], "static");
        assert_eq!(dumped["products"][1]["type"]["library"][0], "automatic");
        assert!(dumped["products"][2]["type"]["executable"].is_null());

        // Targets & settings match SwiftPM's _0 tuple payloads and condition objects
        let kernel_target = &dumped["targets"][0];
        assert_eq!(kernel_target["type"], "regular");
        assert_eq!(
            kernel_target["dependencies"][0]["product"],
            json!(["PicoStdlib", "pico-sdk", null, { "platformNames": ["macos"] }])
        );
        assert_eq!(
            kernel_target["settings"][2],
            json!({
                "condition": {
                    "config": "debug",
                    "platformNames": ["linux"]
                },
                "kind": {
                    "define": { "_0": "PICO_RP2350=1" }
                },
                "tool": "cxx"
            })
        );
    }

    #[test]
    fn test_emit_swift_ast_json_matches_swift_frontend_schema() {
        let src = r#"
protocol Package {
    var name: String { get }
}
struct Shizuku: Package {
    let name: String = "Shizuku"
    func add(_ x: Int, to y: Int) -> Int {
        return x + y
    }
}
"#;
        let ast_json = emit_swift_ast_json(src, "Package.swift").unwrap();
        assert_eq!(ast_json["_kind"], "source_file");
        assert_eq!(ast_json["filename"], "Package.swift");
        assert_eq!(ast_json["items"][0]["_kind"], "protocol");
        assert_eq!(ast_json["items"][0]["name"]["base_name"]["name"], "Package");
        assert_eq!(ast_json["items"][1]["_kind"], "struct_decl");
        assert_eq!(ast_json["items"][1]["name"]["base_name"]["name"], "Shizuku");
        assert_eq!(
            ast_json["items"][1]["members"][2]["_kind"],
            "func_decl"
        );
        assert_eq!(
            ast_json["items"][1]["members"][2]["name"]["args"],
            json!(["", "to"])
        );
    }

    #[test]
    fn test_ada_range_subtype_and_memory_safety_borrow_checker() {
        let mut checker = AdaSafetyChecker::default();
        checker.register_range_subtype(RangeSubtypeSpec {
            name: "ObjectId".to_string(),
            base_type: "Int".to_string(),
            lower_bound: 0,
            upper_bound: 63,
            is_distinct: true,
            span: Span { line: 1, column: 1 },
        });
        checker.register_range_subtype(RangeSubtypeSpec {
            name: "ThreadId".to_string(),
            base_type: "Int".to_string(),
            lower_bound: 0,
            upper_bound: 31,
            is_distinct: true,
            span: Span { line: 2, column: 1 },
        });

        let s = Span { line: 10, column: 5 };
        // Valid ObjectId in 0...63
        assert!(
            checker
                .check_binding("obj_a", true, Some("ObjectId"), &SwiftExpr::Int(42, s), s)
                .is_ok()
        );
        // Out-of-range ObjectId (64 > 63) -> compile-time error!
        let range_err = checker
            .check_binding("obj_bad", false, Some("ObjectId"), &SwiftExpr::Int(64, s), s)
            .unwrap_err();
        assert!(range_err.message.contains("Ada range check failed"));

        // Distinct type mismatch: assigning ObjectId variable to ThreadId -> compile-time error!
        let distinct_err = checker
            .check_binding(
                "thr_bad",
                false,
                Some("ThreadId"),
                &SwiftExpr::Ident("obj_a".to_string(), s),
                s,
            )
            .unwrap_err();
        assert!(distinct_err.message.contains("Ada distinct type mismatch"));

        // Register a function with Ada/Swift ownership modes:
        // func transfer(inout src: ObjectId, inout dst: ObjectId, consuming token: ObjectId)
        checker.register_func_signature(
            "transfer",
            vec![
                (
                    "src".to_string(),
                    ParamOwnershipMode::InOut,
                    "ObjectId".to_string(),
                ),
                (
                    "dst".to_string(),
                    ParamOwnershipMode::InOut,
                    "ObjectId".to_string(),
                ),
                (
                    "token".to_string(),
                    ParamOwnershipMode::ConsumingOut,
                    "ObjectId".to_string(),
                ),
            ],
        );
        checker
            .check_binding("obj_b", true, Some("ObjectId"), &SwiftExpr::Int(10, s), s)
            .unwrap();
        checker
            .check_binding("imm_obj", false, Some("ObjectId"), &SwiftExpr::Int(5, s), s)
            .unwrap();

        // 1. Passing immutable `let imm_obj` to `inout` parameter fails
        let imm_err = checker
            .check_call(
                "transfer",
                &[
                    SwiftExpr::Ident("imm_obj".to_string(), s),
                    SwiftExpr::Ident("obj_b".to_string(), s),
                    SwiftExpr::Int(1, s),
                ],
                s,
            )
            .unwrap_err();
        assert!(imm_err.message.contains("cannot pass immutable `let imm_obj` to `inout`"));

        // 2. Passing `obj_a` twice as `inout` (mutable aliasing) fails!
        let alias_err = checker
            .check_call(
                "transfer",
                &[
                    SwiftExpr::Ident("obj_a".to_string(), s),
                    SwiftExpr::Ident("obj_a".to_string(), s),
                    SwiftExpr::Int(1, s),
                ],
                s,
            )
            .unwrap_err();
        assert!(alias_err.message.contains("aliases 'obj_a'"));

        // 3. Valid call where `imm_obj` is consumed by `consuming` parameter `token`
        assert!(
            checker
                .check_call(
                    "transfer",
                    &[
                        SwiftExpr::Ident("obj_a".to_string(), s),
                        SwiftExpr::Ident("obj_b".to_string(), s),
                        SwiftExpr::Ident("imm_obj".to_string(), s),
                    ],
                    s,
                )
                .is_ok()
        );

        // 4. Use-after-move of `imm_obj` fails!
        let use_after_move = checker
            .check_expr_not_consumed(&SwiftExpr::Ident("imm_obj".to_string(), s))
            .unwrap_err();
        assert!(use_after_move.message.contains("use of moved/consumed value 'imm_obj'"));
    }
}
