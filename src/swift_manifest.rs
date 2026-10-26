use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub span: Span,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Package.swift:{}: {}", self.span, self.message)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, PartialEq)]
enum TokenKind {
    Ident(String),
    ShorthandArg(usize), // $0, $1, $2, ...
    StringLit(String),
    IntLit(i64),
    FloatLit(f64),
    BoolLit(bool),
    Nil,
    Import,
    Let,
    Var,
    Func,
    Interface, // `interface` or `protocol`
    Struct,
    Return,
    If,
    Else,
    In,
    Arrow, // ->
    Dot,
    Comma,
    Colon,
    Semicolon,
    Equals,
    EqEq,  // ==
    NotEq, // !=
    Lt,    // <
    Le,    // <=
    Gt,    // >
    Ge,    // >=
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Eof,
}

#[derive(Debug, Clone)]
struct Token {
    kind: TokenKind,
    span: Span,
}

/// Parameter in a `func` or `interface` method declaration (`func scale(_ x: Int, by factor: Int)`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuncParam {
    pub external_label: Option<String>,
    pub name: String,
    pub ty: String,
}

/// Property requirement inside an `interface` / `protocol` declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceProperty {
    pub name: String,
    pub ty: String,
    pub span: Span,
}

/// Method requirement inside an `interface` / `protocol` declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterfaceMethod {
    pub name: String,
    pub params: Vec<FuncParam>,
    pub return_type: Option<String>,
    pub default_body: Option<Vec<SwiftStmt>>,
    pub span: Span,
}

/// Field or property binding in a `struct` declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructField {
    pub name: String,
    pub ty: Option<String>,
    pub default_value: Option<SwiftExpr>,
    pub mutable: bool,
    pub span: Span,
}

/// Top-level or block statement in the extended Swift-like DSL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SwiftStmt {
    Import {
        module: String,
        span: Span,
    },
    Binding {
        name: String,
        mutable: bool,
        type_annotation: Option<String>,
        value: SwiftExpr,
        span: Span,
    },
    InterfaceDecl {
        name: String,
        properties: Vec<InterfaceProperty>,
        methods: Vec<InterfaceMethod>,
        span: Span,
    },
    StructDecl {
        name: String,
        conformances: Vec<String>,
        fields: Vec<StructField>,
        methods: Vec<SwiftStmt>,
        span: Span,
    },
    FuncDecl {
        name: String,
        params: Vec<FuncParam>,
        return_type: Option<String>,
        body: Vec<SwiftStmt>,
        span: Span,
    },
    Return {
        value: Option<SwiftExpr>,
        span: Span,
    },
    Expr(SwiftExpr),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallArg {
    pub label: Option<String>,
    pub value: SwiftExpr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosureParam {
    pub name: String,
    pub ty: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SwiftExpr {
    String(String, Span),
    Int(i64, Span),
    Float(f64, Span),
    Bool(bool, Span),
    Nil(Span),
    Ident(String, Span),
    ShorthandArg(usize, Span),
    Array(Vec<SwiftExpr>, Span),
    Member {
        base: Option<Box<SwiftExpr>>,
        name: String,
        span: Span,
    },
    Call {
        callee: Box<SwiftExpr>,
        args: Vec<CallArg>,
        span: Span,
    },
    Closure {
        params: Vec<ClosureParam>,
        return_type: Option<String>,
        body: Vec<SwiftStmt>,
        uses_shorthand_args: bool,
        max_shorthand_arity: usize,
        span: Span,
    },
    If {
        cond: Box<SwiftExpr>,
        then_branch: Vec<SwiftStmt>,
        else_branch: Vec<SwiftStmt>,
        span: Span,
    },
    Binary {
        op: BinOp,
        left: Box<SwiftExpr>,
        right: Box<SwiftExpr>,
        span: Span,
    },
}

impl SwiftExpr {
    pub fn span(&self) -> Span {
        match self {
            SwiftExpr::String(_, s)
            | SwiftExpr::Int(_, s)
            | SwiftExpr::Float(_, s)
            | SwiftExpr::Bool(_, s)
            | SwiftExpr::Nil(s)
            | SwiftExpr::Ident(_, s)
            | SwiftExpr::ShorthandArg(_, s)
            | SwiftExpr::Array(_, s)
            | SwiftExpr::Member { span: s, .. }
            | SwiftExpr::Call { span: s, .. }
            | SwiftExpr::Closure { span: s, .. }
            | SwiftExpr::If { span: s, .. }
            | SwiftExpr::Binary { span: s, .. } => *s,
        }
    }
}

/// Origin of a discovered package in the runtime (`struct Foo: Package` vs `Package(...)`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageOrigin {
    /// Defined by a type conforming to the `Package` protocol/interface (`struct Shizuku: Package`).
    ProtocolConformance { type_name: String },
    /// Defined by a `Package(...)` initializer expression (`let package = Package(...)`).
    InitializerCall,
}

/// Strongly-typed representation of a parsed package manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageManifest {
    pub tools_version: Option<String>,
    pub name: String,
    #[serde(default = "default_package_origin")]
    pub origin: PackageOrigin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_localization: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<PlatformSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub products: Vec<ProductSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<PackageDependency>,
    #[serde(default)]
    pub targets: Vec<TargetSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c_language_standard: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cxx_language_standard: Option<String>,
}

fn default_package_origin() -> PackageOrigin {
    PackageOrigin::InitializerCall
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformSpec {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_or_board: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductKind {
    Library {
        #[serde(skip_serializing_if = "Option::is_none")]
        linkage: Option<String>,
    },
    Executable,
    Firmware {
        #[serde(skip_serializing_if = "Option::is_none")]
        format: Option<String>,
    },
    Plugin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductSpec {
    pub name: String,
    pub kind: ProductKind,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum PackageDependency {
    Local {
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        path: String,
    },
    Git {
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        url: String,
        requirement: VersionRequirement,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum VersionRequirement {
    Exact(String),
    From(String),
    UpToNextMajor(String),
    UpToNextMinor(String),
    Branch(String),
    Revision(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Regular,
    Executable,
    Test,
    System,
    Binary,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildCondition {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration: Option<String>,
}

impl BuildCondition {
    pub fn matches(&self, platform: Option<&str>, configuration: Option<&str>) -> bool {
        let platform_ok = match platform {
            Some(p) if !self.platforms.is_empty() => self
                .platforms
                .iter()
                .any(|cand| cand.eq_ignore_ascii_case(p)),
            _ => true,
        };
        let config_ok = match (configuration, self.configuration.as_deref()) {
            (Some(c), Some(expected)) => expected.eq_ignore_ascii_case(c),
            _ => true,
        };
        platform_ok && config_ok
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TargetDependency {
    ByName {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<BuildCondition>,
    },
    Target {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<BuildCondition>,
    },
    Product {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        package: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<BuildCondition>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "setting", rename_all = "snake_case")]
pub enum CompilerSetting {
    Define {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<BuildCondition>,
    },
    HeaderSearchPath {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<BuildCondition>,
    },
    Standard {
        standard: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<BuildCondition>,
    },
    UnsafeFlags {
        flags: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<BuildCondition>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "setting", rename_all = "snake_case")]
pub enum LinkerSetting {
    LinkedLibrary {
        library: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<BuildCondition>,
    },
    LinkedFramework {
        framework: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<BuildCondition>,
    },
    GcSections {
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<BuildCondition>,
    },
    LinkerScript {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<BuildCondition>,
    },
    OutputFormat {
        format: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        family: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<BuildCondition>,
    },
    UnsafeFlags {
        flags: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<BuildCondition>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetSpec {
    pub name: String,
    pub kind: TargetKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_headers_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<TargetDependency>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub c_settings: Vec<CompilerSetting>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cxx_settings: Vec<CompilerSetting>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub swift_settings: Vec<CompilerSetting>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linker_settings: Vec<LinkerSetting>,
}

/// Flattened, platform-resolved view of a target ready for command / Ninja generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedTargetPlan {
    pub name: String,
    pub kind: TargetKind,
    pub effective_path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    pub dependencies: Vec<String>,
    pub c_flags: Vec<String>,
    pub cxx_flags: Vec<String>,
    pub linker_flags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
}

impl PackageManifest {
    /// Evaluates `.when(platforms: ..., configuration: ...)` conditions across all targets
    /// and produces concrete compiler/linker flags for the given platform and build configuration.
    pub fn resolve_plan(
        &self,
        platform: Option<&str>,
        configuration: Option<&str>,
    ) -> Vec<ResolvedTargetPlan> {
        self.targets
            .iter()
            .map(|target| {
                let default_prefix = match target.kind {
                    TargetKind::Test => "Tests",
                    _ => "Sources",
                };
                let effective_path = target
                    .path
                    .clone()
                    .unwrap_or_else(|| format!("{default_prefix}/{}", target.name));

                let mut dependencies = Vec::new();
                for dep in &target.dependencies {
                    match dep {
                        TargetDependency::ByName { name, condition }
                        | TargetDependency::Target { name, condition } => {
                            if condition
                                .as_ref()
                                .is_none_or(|c| c.matches(platform, configuration))
                            {
                                dependencies.push(name.clone());
                            }
                        }
                        TargetDependency::Product {
                            name,
                            package,
                            condition,
                        } => {
                            if condition
                                .as_ref()
                                .is_none_or(|c| c.matches(platform, configuration))
                            {
                                if let Some(pkg) = package {
                                    dependencies.push(format!("{pkg}:{name}"));
                                } else {
                                    dependencies.push(name.clone());
                                }
                            }
                        }
                    }
                }

                let c_flags = resolve_compiler_settings(
                    &target.c_settings,
                    self.c_language_standard.as_deref(),
                    platform,
                    configuration,
                );
                let cxx_flags = resolve_compiler_settings(
                    &target.cxx_settings,
                    self.cxx_language_standard.as_deref(),
                    platform,
                    configuration,
                );

                let mut linker_flags = Vec::new();
                let mut output_format = None;
                for setting in &target.linker_settings {
                    match setting {
                        LinkerSetting::LinkedLibrary { library, condition } => {
                            if condition
                                .as_ref()
                                .is_none_or(|c| c.matches(platform, configuration))
                            {
                                linker_flags.push(format!("-l{library}"));
                            }
                        }
                        LinkerSetting::LinkedFramework {
                            framework,
                            condition,
                        } => {
                            if condition
                                .as_ref()
                                .is_none_or(|c| c.matches(platform, configuration))
                            {
                                linker_flags.push(format!("-framework {framework}"));
                            }
                        }
                        LinkerSetting::GcSections { condition } => {
                            if condition
                                .as_ref()
                                .is_none_or(|c| c.matches(platform, configuration))
                            {
                                linker_flags.push("-Wl,--gc-sections".to_string());
                            }
                        }
                        LinkerSetting::LinkerScript { path, condition } => {
                            if condition
                                .as_ref()
                                .is_none_or(|c| c.matches(platform, configuration))
                            {
                                linker_flags.push(format!("-T{path}"));
                            }
                        }
                        LinkerSetting::OutputFormat {
                            format,
                            family,
                            condition,
                        } => {
                            if condition
                                .as_ref()
                                .is_none_or(|c| c.matches(platform, configuration))
                            {
                                output_format = Some(match family {
                                    Some(fam) => format!("{format} ({fam})"),
                                    None => format.clone(),
                                });
                            }
                        }
                        LinkerSetting::UnsafeFlags { flags, condition } => {
                            if condition
                                .as_ref()
                                .is_none_or(|c| c.matches(platform, configuration))
                            {
                                linker_flags.extend(flags.iter().cloned());
                            }
                        }
                    }
                }

                ResolvedTargetPlan {
                    name: target.name.clone(),
                    kind: target.kind.clone(),
                    effective_path,
                    sources: target.sources.clone(),
                    dependencies,
                    c_flags,
                    cxx_flags,
                    linker_flags,
                    output_format,
                }
            })
            .collect()
    }

    /// Generates a standalone `build.ninja` file from the resolved plan so builds can run
    /// with zero JVM or `swiftc` overhead.
    pub fn emit_build_ninja(&self, platform: Option<&str>, configuration: Option<&str>) -> String {
        let plan = self.resolve_plan(platform, configuration);
        let is_rp2350 = platform.is_some_and(|p| p.eq_ignore_ascii_case("rp2350"));
        let cxx = if is_rp2350 {
            "arm-none-eabi-g++"
        } else {
            "clang++"
        };
        let ar = if is_rp2350 { "arm-none-eabi-ar" } else { "ar" };

        let mut out = String::new();
        let _ = writeln!(
            out,
            "# Auto-generated by rbuild manifest compiler (package: {})",
            self.name
        );
        let _ = writeln!(out, "builddir = .rbuild-out/ninja");
        let _ = writeln!(out, "cxx = {cxx}");
        let _ = writeln!(out, "ar = {ar}\n");
        let _ = writeln!(out, "rule cxx");
        let _ = writeln!(
            out,
            "  command = $cxx -MMD -MF $out.d $cxxflags -c $in -o $out"
        );
        let _ = writeln!(out, "  depfile = $out.d");
        let _ = writeln!(out, "  deps = gcc");
        let _ = writeln!(out, "  description = CXX $out\n");

        let _ = writeln!(out, "rule ar");
        let _ = writeln!(out, "  command = rm -f $out && $ar rcs $out $in");
        let _ = writeln!(out, "  description = AR $out\n");

        let _ = writeln!(out, "rule link");
        let _ = writeln!(out, "  command = $cxx $in $ldflags -o $out");
        let _ = writeln!(out, "  description = LINK $out\n");

        let _ = writeln!(out, "rule uf2");
        let _ = writeln!(
            out,
            "  command = picotool uf2 convert --quiet $in -t elf $out --family $family"
        );
        let _ = writeln!(out, "  description = UF2 $out\n");

        let mut regen_flags = String::new();
        if let Some(p) = platform {
            let _ = write!(regen_flags, " --platform {p}");
        }
        if let Some(c) = configuration {
            let _ = write!(regen_flags, " --configuration {c}");
        }
        let _ = writeln!(out, "rule regen");
        let _ = writeln!(
            out,
            "  command = rbuild parse-swift $in{regen_flags} --emit-ninja > $out"
        );
        let _ = writeln!(out, "  generator = 1");
        let _ = writeln!(out, "  restat = 1");
        let _ = writeln!(out, "  description = REGEN $out\n");

        let mut watched_dirs = std::collections::BTreeSet::new();
        for (target_spec, target) in self.targets.iter().zip(plan.iter()) {
            watched_dirs.insert(target.effective_path.clone());
            let sources = if target.sources.is_empty()
                || target
                    .sources
                    .iter()
                    .any(|s| s.contains('*') || s.contains('?'))
            {
                let expanded = crate::executor::expand_target_sources(
                    std::path::Path::new("."),
                    &target.effective_path,
                    &target.sources,
                    &target_spec.exclude,
                );
                if expanded.len() == 1
                    && target.sources.is_empty()
                    && !std::path::Path::new(&target.effective_path).is_dir()
                {
                    vec![format!("{}/{}.cpp", target.effective_path, target.name)]
                } else {
                    expanded
                }
            } else {
                target
                    .sources
                    .iter()
                    .map(|s| format!("{}/{s}", target.effective_path))
                    .collect()
            };
            let mut objs = Vec::new();
            for src in &sources {
                let obj = format!("$builddir/obj/{}/{}.o", target.name, src.replace('/', "_"));
                let _ = writeln!(out, "build {obj}: cxx {src}");
                if !target.cxx_flags.is_empty() {
                    let _ = writeln!(out, "  cxxflags = {}", target.cxx_flags.join(" "));
                }
                objs.push(obj);
            }

            match target.kind {
                TargetKind::Regular => {
                    let lib_out = format!("$builddir/lib{}.a", target.name);
                    let _ = writeln!(out, "build {lib_out}: ar {}\n", objs.join(" "));
                }
                TargetKind::Executable | TargetKind::Test => {
                    let mut link_inputs = objs;
                    for dep in &target.dependencies {
                        if !dep.contains(':') {
                            link_inputs.push(format!("$builddir/lib{dep}.a"));
                        }
                    }
                    let bin_out = if is_rp2350 {
                        format!("$builddir/{}.elf", target.name)
                    } else {
                        format!("$builddir/{}", target.name)
                    };
                    let _ = writeln!(out, "build {bin_out}: link {}", link_inputs.join(" "));
                    if !target.linker_flags.is_empty() {
                        let _ = writeln!(out, "  ldflags = {}", target.linker_flags.join(" "));
                    }
                    if let Some(fmt) = &target.output_format {
                        if fmt.starts_with("uf2") {
                            let uf2_out = format!("$builddir/{}.uf2", target.name);
                            let _ = writeln!(out, "build {uf2_out}: uf2 {bin_out}");
                            let _ = writeln!(out, "  family = rp2350-arm-s");
                        }
                    }
                    let _ = writeln!(out);
                }
                _ => {}
            }
        }
        let implicit_dirs: Vec<String> = watched_dirs.into_iter().collect();
        if implicit_dirs.is_empty() {
            let _ = writeln!(out, "build build.ninja: regen Package.swift");
        } else {
            let _ = writeln!(
                out,
                "build build.ninja: regen Package.swift | {}",
                implicit_dirs.join(" ")
            );
        }
        out
    }
}

fn resolve_compiler_settings(
    settings: &[CompilerSetting],
    package_default_std: Option<&str>,
    platform: Option<&str>,
    configuration: Option<&str>,
) -> Vec<String> {
    let mut flags = Vec::new();
    let mut has_explicit_std = false;
    for setting in settings {
        match setting {
            CompilerSetting::Define {
                name,
                value,
                condition,
            } => {
                if condition
                    .as_ref()
                    .is_none_or(|c| c.matches(platform, configuration))
                {
                    match value {
                        Some(v) => flags.push(format!("-D{name}={v}")),
                        None => flags.push(format!("-D{name}")),
                    }
                }
            }
            CompilerSetting::HeaderSearchPath { path, condition } => {
                if condition
                    .as_ref()
                    .is_none_or(|c| c.matches(platform, configuration))
                {
                    flags.push(format!("-I{path}"));
                }
            }
            CompilerSetting::Standard {
                standard,
                condition,
            } => {
                if condition
                    .as_ref()
                    .is_none_or(|c| c.matches(platform, configuration))
                {
                    has_explicit_std = true;
                    flags.push(format!("-std={}", normalize_standard_flag(standard)));
                }
            }
            CompilerSetting::UnsafeFlags {
                flags: raw,
                condition,
            } => {
                if condition
                    .as_ref()
                    .is_none_or(|c| c.matches(platform, configuration))
                {
                    flags.extend(raw.iter().cloned());
                }
            }
        }
    }
    if !has_explicit_std {
        if let Some(std_name) = package_default_std {
            flags.insert(0, format!("-std={}", normalize_standard_flag(std_name)));
        }
    }
    flags
}

fn normalize_standard_flag(std_name: &str) -> String {
    if let Some(rest) = std_name.strip_prefix("cxx") {
        format!("c++{rest}")
    } else if let Some(rest) = std_name.strip_prefix("gnu_cxx") {
        format!("gnu++{rest}")
    } else {
        std_name.replace('_', "")
    }
}

/// Parses a source string into an untyped AST of [`SwiftStmt`] nodes.
pub fn parse_swift_ast(source: &str) -> Result<Vec<SwiftStmt>, ParseError> {
    let tokens = tokenize(source)?;
    let mut parser = Parser::new(tokens);
    parser.parse_program()
}

/// Discovers and evaluates all packages in a manifest source file:
/// - Any `struct <Name>: Package { ... }` conforming to the `Package` protocol/interface
/// - Any `let package = Package(...)` or top-level `Package(...)` initializer call
pub fn parse_packages_swift(source: &str) -> Result<Vec<PackageManifest>, ParseError> {
    let tools_version = extract_swift_tools_version(source);
    let stmts = parse_swift_ast(source)?;
    evaluate_all_packages(&stmts, tools_version)
}

/// Parses a `Package.swift` source string directly into the primary [`PackageManifest`]
/// without invoking `swiftc`.
pub fn parse_package_swift(source: &str) -> Result<PackageManifest, ParseError> {
    let mut pkgs = parse_packages_swift(source)?;
    pkgs.drain(..).next().ok_or_else(|| ParseError {
        span: Span { line: 1, column: 1 },
        message:
            "no `struct <Name>: Package` conformance or `Package(...)` initializer found in manifest"
                .to_string(),
    })
}

/// Extracts `// swift-tools-version: X.Y` from the first non-empty lines of the file.
pub fn extract_swift_tools_version(source: &str) -> Option<String> {
    for line in source.lines().take(5) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("//") {
            let rest = rest.trim();
            if let Some(ver) = rest.strip_prefix("swift-tools-version:") {
                let ver_clean = ver.split(';').next().unwrap_or(ver).trim();
                if !ver_clean.is_empty() {
                    return Some(ver_clean.to_string());
                }
            }
        } else {
            break;
        }
    }
    None
}

fn tokenize(source: &str) -> Result<Vec<Token>, ParseError> {
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    let mut line = 1;
    let mut col = 1;
    let mut tokens = Vec::new();

    let advance = |i: &mut usize, line: &mut usize, col: &mut usize, ch: char| {
        *i += 1;
        if ch == '\n' {
            *line += 1;
            *col = 1;
        } else {
            *col += 1;
        }
    };

    while i < chars.len() {
        let ch = chars[i];
        let span = Span { line, column: col };

        if ch.is_whitespace() {
            advance(&mut i, &mut line, &mut col, ch);
            continue;
        }

        // Line comment `// ...`
        if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            while i < chars.len() && chars[i] != '\n' {
                let c = chars[i];
                advance(&mut i, &mut line, &mut col, c);
            }
            continue;
        }

        // Block comment `/* ... */` (supports nesting like Swift)
        if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            let start_span = span;
            advance(&mut i, &mut line, &mut col, '/');
            advance(&mut i, &mut line, &mut col, '*');
            let mut depth = 1usize;
            while i < chars.len() && depth > 0 {
                if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
                    depth += 1;
                    advance(&mut i, &mut line, &mut col, '/');
                    advance(&mut i, &mut line, &mut col, '*');
                } else if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                    depth -= 1;
                    advance(&mut i, &mut line, &mut col, '*');
                    advance(&mut i, &mut line, &mut col, '/');
                } else {
                    let c = chars[i];
                    advance(&mut i, &mut line, &mut col, c);
                }
            }
            if depth > 0 {
                return Err(ParseError {
                    span: start_span,
                    message: "unterminated block comment".to_string(),
                });
            }
            continue;
        }

        // Two-character operators: `->`, `==`, `!=`, `<=`, `>=`
        if ch == '-' && i + 1 < chars.len() && chars[i + 1] == '>' {
            advance(&mut i, &mut line, &mut col, '-');
            advance(&mut i, &mut line, &mut col, '>');
            tokens.push(Token {
                kind: TokenKind::Arrow,
                span,
            });
            continue;
        }
        if ch == '=' && i + 1 < chars.len() && chars[i + 1] == '=' {
            advance(&mut i, &mut line, &mut col, '=');
            advance(&mut i, &mut line, &mut col, '=');
            tokens.push(Token {
                kind: TokenKind::EqEq,
                span,
            });
            continue;
        }
        if ch == '!' && i + 1 < chars.len() && chars[i + 1] == '=' {
            advance(&mut i, &mut line, &mut col, '!');
            advance(&mut i, &mut line, &mut col, '=');
            tokens.push(Token {
                kind: TokenKind::NotEq,
                span,
            });
            continue;
        }
        if ch == '<' && i + 1 < chars.len() && chars[i + 1] == '=' {
            advance(&mut i, &mut line, &mut col, '<');
            advance(&mut i, &mut line, &mut col, '=');
            tokens.push(Token {
                kind: TokenKind::Le,
                span,
            });
            continue;
        }
        if ch == '>' && i + 1 < chars.len() && chars[i + 1] == '=' {
            advance(&mut i, &mut line, &mut col, '>');
            advance(&mut i, &mut line, &mut col, '=');
            tokens.push(Token {
                kind: TokenKind::Ge,
                span,
            });
            continue;
        }

        // Shorthand closure parameter `$0`, `$1`, ...
        if ch == '$' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
            advance(&mut i, &mut line, &mut col, '$');
            let mut digits = String::new();
            while i < chars.len() && chars[i].is_ascii_digit() {
                let d = chars[i];
                advance(&mut i, &mut line, &mut col, d);
                digits.push(d);
            }
            let idx = digits.parse::<usize>().map_err(|_| ParseError {
                span,
                message: format!("invalid shorthand argument '${digits}'"),
            })?;
            tokens.push(Token {
                kind: TokenKind::ShorthandArg(idx),
                span,
            });
            continue;
        }

        match ch {
            '.' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::Dot,
                    span,
                });
            }
            ',' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::Comma,
                    span,
                });
            }
            ':' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::Colon,
                    span,
                });
            }
            ';' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::Semicolon,
                    span,
                });
            }
            '=' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::Equals,
                    span,
                });
            }
            '<' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::Lt,
                    span,
                });
            }
            '>' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::Gt,
                    span,
                });
            }
            '+' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::Plus,
                    span,
                });
            }
            '-' if !(i + 1 < chars.len() && chars[i + 1].is_ascii_digit()) => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::Minus,
                    span,
                });
            }
            '*' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::Star,
                    span,
                });
            }
            '/' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::Slash,
                    span,
                });
            }
            '(' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::LParen,
                    span,
                });
            }
            ')' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::RParen,
                    span,
                });
            }
            '[' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::LBracket,
                    span,
                });
            }
            ']' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::RBracket,
                    span,
                });
            }
            '{' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::LBrace,
                    span,
                });
            }
            '}' => {
                advance(&mut i, &mut line, &mut col, ch);
                tokens.push(Token {
                    kind: TokenKind::RBrace,
                    span,
                });
            }
            '"' => {
                advance(&mut i, &mut line, &mut col, '"');
                let mut value = String::new();
                let mut closed = false;
                while i < chars.len() {
                    let c = chars[i];
                    if c == '"' {
                        advance(&mut i, &mut line, &mut col, '"');
                        closed = true;
                        break;
                    } else if c == '\\' {
                        advance(&mut i, &mut line, &mut col, '\\');
                        if i >= chars.len() {
                            break;
                        }
                        let esc = chars[i];
                        advance(&mut i, &mut line, &mut col, esc);
                        match esc {
                            '"' => value.push('"'),
                            '\\' => value.push('\\'),
                            'n' => value.push('\n'),
                            'r' => value.push('\r'),
                            't' => value.push('\t'),
                            other => value.push(other),
                        }
                    } else {
                        advance(&mut i, &mut line, &mut col, c);
                        value.push(c);
                    }
                }
                if !closed {
                    return Err(ParseError {
                        span,
                        message: "unterminated string literal".to_string(),
                    });
                }
                tokens.push(Token {
                    kind: TokenKind::StringLit(value),
                    span,
                });
            }
            '`' => {
                advance(&mut i, &mut line, &mut col, '`');
                let mut ident = String::new();
                while i < chars.len() && chars[i] != '`' {
                    let c = chars[i];
                    advance(&mut i, &mut line, &mut col, c);
                    ident.push(c);
                }
                if i >= chars.len() {
                    return Err(ParseError {
                        span,
                        message: "unterminated backtick identifier".to_string(),
                    });
                }
                advance(&mut i, &mut line, &mut col, '`');
                tokens.push(Token {
                    kind: TokenKind::Ident(ident),
                    span,
                });
            }
            c if c.is_ascii_digit()
                || (c == '-' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit()) =>
            {
                let mut num = String::new();
                if c == '-' {
                    advance(&mut i, &mut line, &mut col, '-');
                    num.push('-');
                }
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '_') {
                    let d = chars[i];
                    advance(&mut i, &mut line, &mut col, d);
                    if d != '_' {
                        num.push(d);
                    }
                }
                let mut is_float = false;
                if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
                    is_float = true;
                    advance(&mut i, &mut line, &mut col, '.');
                    num.push('.');
                    while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '_') {
                        let d = chars[i];
                        advance(&mut i, &mut line, &mut col, d);
                        if d != '_' {
                            num.push(d);
                        }
                    }
                }
                if is_float {
                    let parsed = num.parse::<f64>().map_err(|_| ParseError {
                        span,
                        message: format!("invalid float literal '{num}'"),
                    })?;
                    tokens.push(Token {
                        kind: TokenKind::FloatLit(parsed),
                        span,
                    });
                } else {
                    let parsed = num.parse::<i64>().map_err(|_| ParseError {
                        span,
                        message: format!("invalid integer literal '{num}'"),
                    })?;
                    tokens.push(Token {
                        kind: TokenKind::IntLit(parsed),
                        span,
                    });
                }
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut ident = String::new();
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    let ch_i = chars[i];
                    advance(&mut i, &mut line, &mut col, ch_i);
                    ident.push(ch_i);
                }
                let kind = match ident.as_str() {
                    "import" => TokenKind::Import,
                    "let" => TokenKind::Let,
                    "var" => TokenKind::Var,
                    "func" => TokenKind::Func,
                    "interface" | "protocol" => TokenKind::Interface,
                    "struct" => TokenKind::Struct,
                    "return" => TokenKind::Return,
                    "if" => TokenKind::If,
                    "else" => TokenKind::Else,
                    "in" => TokenKind::In,
                    "true" => TokenKind::BoolLit(true),
                    "false" => TokenKind::BoolLit(false),
                    "nil" => TokenKind::Nil,
                    _ => TokenKind::Ident(ident),
                };
                tokens.push(Token { kind, span });
            }
            other => {
                return Err(ParseError {
                    span,
                    message: format!("unexpected character '{other}'"),
                });
            }
        }
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span { line, column: col },
    });
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    allow_trailing_closure: bool,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            pos: 0,
            allow_trailing_closure: true,
        }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn peek_n(&self, offset: usize) -> &Token {
        let idx = (self.pos + offset).min(self.tokens.len() - 1);
        &self.tokens[idx]
    }

    fn bump(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn skip_semicolons(&mut self) {
        while self.peek().kind == TokenKind::Semicolon {
            self.bump();
        }
    }

    fn expect(&mut self, expected: &TokenKind, context: &str) -> Result<Span, ParseError> {
        let tok = self.bump();
        if &tok.kind == expected {
            Ok(tok.span)
        } else {
            Err(ParseError {
                span: tok.span,
                message: format!("expected {context} ({expected:?}), found {:?}", tok.kind),
            })
        }
    }

    fn expect_ident(&mut self, context: &str) -> Result<(String, Span), ParseError> {
        let tok = self.bump();
        match tok.kind {
            TokenKind::Ident(name) => Ok((name, tok.span)),
            TokenKind::Import => Ok(("import".to_string(), tok.span)),
            TokenKind::Let => Ok(("let".to_string(), tok.span)),
            TokenKind::Var => Ok(("var".to_string(), tok.span)),
            TokenKind::Func => Ok(("func".to_string(), tok.span)),
            TokenKind::Interface => Ok(("interface".to_string(), tok.span)),
            TokenKind::Struct => Ok(("struct".to_string(), tok.span)),
            TokenKind::In => Ok(("in".to_string(), tok.span)),
            other => Err(ParseError {
                span: tok.span,
                message: format!("expected {context}, found {other:?}"),
            }),
        }
    }

    fn parse_program(&mut self) -> Result<Vec<SwiftStmt>, ParseError> {
        let mut stmts = Vec::new();
        self.skip_semicolons();
        while self.peek().kind != TokenKind::Eof {
            stmts.push(self.parse_stmt()?);
            self.skip_semicolons();
        }
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<SwiftStmt, ParseError> {
        match &self.peek().kind {
            TokenKind::Import => {
                let tok = self.bump();
                let (module, _) = self.expect_ident("module name after 'import'")?;
                Ok(SwiftStmt::Import {
                    module,
                    span: tok.span,
                })
            }
            TokenKind::Let | TokenKind::Var => {
                let kw = self.bump();
                let mutable = kw.kind == TokenKind::Var;
                let (name, _) = self.expect_ident("variable name after 'let'/'var'")?;
                let type_annotation = if self.peek().kind == TokenKind::Colon {
                    self.bump();
                    Some(self.parse_type_annotation()?)
                } else {
                    None
                };
                self.expect(&TokenKind::Equals, "'=' in variable binding")?;
                let value = self.parse_expr()?;
                Ok(SwiftStmt::Binding {
                    name,
                    mutable,
                    type_annotation,
                    value,
                    span: kw.span,
                })
            }
            TokenKind::Interface => self.parse_interface_decl(),
            TokenKind::Struct => self.parse_struct_decl(),
            TokenKind::Func => self.parse_func_decl(),
            TokenKind::Return => {
                let kw = self.bump();
                if matches!(
                    self.peek().kind,
                    TokenKind::RBrace | TokenKind::Semicolon | TokenKind::Eof
                ) {
                    Ok(SwiftStmt::Return {
                        value: None,
                        span: kw.span,
                    })
                } else {
                    let value = self.parse_expr()?;
                    Ok(SwiftStmt::Return {
                        value: Some(value),
                        span: kw.span,
                    })
                }
            }
            _ => {
                let expr = self.parse_expr()?;
                Ok(SwiftStmt::Expr(expr))
            }
        }
    }

    fn parse_interface_decl(&mut self) -> Result<SwiftStmt, ParseError> {
        let kw = self.bump(); // `interface` or `protocol`
        let (name, _) = self.expect_ident("interface name")?;
        self.expect(&TokenKind::LBrace, "'{' after interface name")?;
        let mut properties = Vec::new();
        let mut methods = Vec::new();
        self.skip_semicolons();
        while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::Eof {
            match &self.peek().kind {
                TokenKind::Let | TokenKind::Var => {
                    let p_tok = self.bump();
                    let (p_name, _) = self.expect_ident("property name in interface")?;
                    self.expect(&TokenKind::Colon, "':' after interface property name")?;
                    let ty = self.parse_type_annotation()?;
                    // Optional `{ get }` or `{ get set }` block in Swift protocols
                    if self.peek().kind == TokenKind::LBrace {
                        self.bump();
                        while self.peek().kind != TokenKind::RBrace
                            && self.peek().kind != TokenKind::Eof
                        {
                            self.bump();
                        }
                        self.expect(&TokenKind::RBrace, "'}' after property accessor spec")?;
                    }
                    properties.push(InterfaceProperty {
                        name: p_name,
                        ty,
                        span: p_tok.span,
                    });
                }
                TokenKind::Func => {
                    let fn_tok = self.bump();
                    let (m_name, _) = self.expect_ident("method name")?;
                    let params = self.parse_func_params()?;
                    let return_type = if self.peek().kind == TokenKind::Arrow {
                        self.bump();
                        Some(self.parse_type_annotation()?)
                    } else {
                        None
                    };
                    let default_body = if self.peek().kind == TokenKind::LBrace {
                        Some(self.parse_block_stmts()?)
                    } else {
                        None
                    };
                    methods.push(InterfaceMethod {
                        name: m_name,
                        params,
                        return_type,
                        default_body,
                        span: fn_tok.span,
                    });
                }
                other => {
                    return Err(ParseError {
                        span: self.peek().span,
                        message: format!(
                            "expected `func`, `let`, or `var` inside interface '{name}', found {other:?}"
                        ),
                    });
                }
            }
            self.skip_semicolons();
        }
        self.expect(&TokenKind::RBrace, "'}' closing interface declaration")?;
        Ok(SwiftStmt::InterfaceDecl {
            name,
            properties,
            methods,
            span: kw.span,
        })
    }

    fn parse_struct_decl(&mut self) -> Result<SwiftStmt, ParseError> {
        let kw = self.bump(); // `struct`
        let (name, _) = self.expect_ident("struct name")?;
        let mut conformances = Vec::new();
        if self.peek().kind == TokenKind::Colon {
            self.bump();
            loop {
                let (iface, _) = self.expect_ident("conformed interface name")?;
                conformances.push(iface);
                if self.peek().kind == TokenKind::Comma {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.expect(&TokenKind::LBrace, "'{' opening struct body")?;
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        self.skip_semicolons();
        while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::Eof {
            match &self.peek().kind {
                TokenKind::Let | TokenKind::Var => {
                    let tok = self.bump();
                    let mutable = tok.kind == TokenKind::Var;
                    let (f_name, f_span) = self.expect_ident("field name")?;
                    let ty = if self.peek().kind == TokenKind::Colon {
                        self.bump();
                        Some(self.parse_type_annotation()?)
                    } else {
                        None
                    };
                    let default_value = if self.peek().kind == TokenKind::Equals {
                        self.bump();
                        Some(self.parse_expr()?)
                    } else if self.peek().kind == TokenKind::LBrace {
                        // Computed property shorthand: `var targets: [Target] { [...] }`
                        let body = self.parse_block_stmts()?;
                        let expr = match body.into_iter().last() {
                            Some(SwiftStmt::Return { value: Some(v), .. }) => v,
                            Some(SwiftStmt::Expr(e)) => e,
                            _ => SwiftExpr::Nil(f_span),
                        };
                        Some(expr)
                    } else {
                        None
                    };
                    if ty.is_none() && default_value.is_none() {
                        return Err(ParseError {
                            span: f_span,
                            message: format!(
                                "field '{f_name}' in struct '{name}' requires a type annotation `: Type` or initializer `= ...`"
                            ),
                        });
                    }
                    fields.push(StructField {
                        name: f_name,
                        ty,
                        default_value,
                        mutable,
                        span: f_span,
                    });
                }
                TokenKind::Func => {
                    methods.push(self.parse_func_decl()?);
                }
                other => {
                    return Err(ParseError {
                        span: self.peek().span,
                        message: format!(
                            "expected `let`, `var`, or `func` inside struct '{name}', found {other:?}"
                        ),
                    });
                }
            }
            self.skip_semicolons();
        }
        self.expect(&TokenKind::RBrace, "'}' closing struct body")?;
        Ok(SwiftStmt::StructDecl {
            name,
            conformances,
            fields,
            methods,
            span: kw.span,
        })
    }

    fn parse_func_decl(&mut self) -> Result<SwiftStmt, ParseError> {
        let kw = self.bump(); // `func`
        let (name, _) = self.expect_ident("function name")?;
        let params = self.parse_func_params()?;
        let return_type = if self.peek().kind == TokenKind::Arrow {
            self.bump();
            Some(self.parse_type_annotation()?)
        } else {
            None
        };
        let body = self.parse_block_stmts()?;
        Ok(SwiftStmt::FuncDecl {
            name,
            params,
            return_type,
            body,
            span: kw.span,
        })
    }

    fn parse_func_params(&mut self) -> Result<Vec<FuncParam>, ParseError> {
        self.expect(&TokenKind::LParen, "'(' for parameter list")?;
        let mut params = Vec::new();
        while self.peek().kind != TokenKind::RParen && self.peek().kind != TokenKind::Eof {
            let (first, _) = self.expect_ident("parameter label or name")?;
            let (external_label, name) = if self.peek().kind == TokenKind::Colon {
                (Some(first.clone()), first)
            } else {
                let (second, _) = self.expect_ident("internal parameter name")?;
                let ext = if first == "_" { None } else { Some(first) };
                (ext, second)
            };
            self.expect(&TokenKind::Colon, "':' before parameter type")?;
            let ty = self.parse_type_annotation()?;
            params.push(FuncParam {
                external_label,
                name,
                ty,
            });
            if self.peek().kind == TokenKind::Comma {
                self.bump();
            } else {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "')' closing parameter list")?;
        Ok(params)
    }

    fn parse_block_stmts(&mut self) -> Result<Vec<SwiftStmt>, ParseError> {
        self.expect(&TokenKind::LBrace, "'{' opening block")?;
        let mut stmts = Vec::new();
        self.skip_semicolons();
        while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::Eof {
            stmts.push(self.parse_stmt()?);
            self.skip_semicolons();
        }
        self.expect(&TokenKind::RBrace, "'}' closing block")?;
        Ok(stmts)
    }

    fn parse_type_annotation(&mut self) -> Result<String, ParseError> {
        if self.peek().kind == TokenKind::LBracket {
            self.bump();
            let inner = self.parse_type_annotation()?;
            self.expect(&TokenKind::RBracket, "']' in array type annotation")?;
            return Ok(format!("[{inner}]"));
        }
        let (mut name, _) = self.expect_ident("type name")?;
        while self.peek().kind == TokenKind::Dot {
            self.bump();
            let (member, _) = self.expect_ident("nested type name")?;
            name.push('.');
            name.push_str(&member);
        }
        Ok(name)
    }

    fn parse_expr(&mut self) -> Result<SwiftExpr, ParseError> {
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<SwiftExpr, ParseError> {
        let mut left = self.parse_additive()?;
        while let Some(op) = match self.peek().kind {
            TokenKind::EqEq => Some(BinOp::Eq),
            TokenKind::NotEq => Some(BinOp::NotEq),
            TokenKind::Lt => Some(BinOp::Lt),
            TokenKind::Le => Some(BinOp::Le),
            TokenKind::Gt => Some(BinOp::Gt),
            TokenKind::Ge => Some(BinOp::Ge),
            _ => None,
        } {
            let op_tok = self.bump();
            let right = self.parse_additive()?;
            left = SwiftExpr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span: op_tok.span,
            };
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<SwiftExpr, ParseError> {
        let mut left = self.parse_multiplicative()?;
        while let Some(op) = match self.peek().kind {
            TokenKind::Plus => Some(BinOp::Add),
            TokenKind::Minus => Some(BinOp::Sub),
            _ => None,
        } {
            let op_tok = self.bump();
            let right = self.parse_multiplicative()?;
            left = SwiftExpr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span: op_tok.span,
            };
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<SwiftExpr, ParseError> {
        let mut left = self.parse_postfix()?;
        while let Some(op) = match self.peek().kind {
            TokenKind::Star => Some(BinOp::Mul),
            TokenKind::Slash => Some(BinOp::Div),
            _ => None,
        } {
            let op_tok = self.bump();
            let right = self.parse_postfix()?;
            left = SwiftExpr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span: op_tok.span,
            };
        }
        Ok(left)
    }

    fn parse_postfix(&mut self) -> Result<SwiftExpr, ParseError> {
        let mut expr = self.parse_primary()?;
        loop {
            match &self.peek().kind {
                TokenKind::Dot => {
                    let dot = self.bump();
                    let (member, _) = self.expect_ident("member name after '.'")?;
                    expr = SwiftExpr::Member {
                        base: Some(Box::new(expr)),
                        name: member,
                        span: dot.span,
                    };
                }
                TokenKind::LParen => {
                    let lparen = self.bump();
                    let mut args = self.parse_call_args()?;
                    if self.allow_trailing_closure && self.peek().kind == TokenKind::LBrace {
                        let closure = self.parse_closure_expr()?;
                        let c_span = closure.span();
                        args.push(CallArg {
                            label: None,
                            value: closure,
                            span: c_span,
                        });
                    }
                    expr = SwiftExpr::Call {
                        callee: Box::new(expr),
                        args,
                        span: lparen.span,
                    };
                }
                TokenKind::LBrace
                    if self.allow_trailing_closure
                        && matches!(expr, SwiftExpr::Member { .. } | SwiftExpr::Ident(_, _)) =>
                {
                    let closure = self.parse_closure_expr()?;
                    let c_span = closure.span();
                    expr = SwiftExpr::Call {
                        callee: Box::new(expr),
                        args: vec![CallArg {
                            label: None,
                            value: closure,
                            span: c_span,
                        }],
                        span: c_span,
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<SwiftExpr, ParseError> {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::StringLit(s) => {
                self.bump();
                Ok(SwiftExpr::String(s, tok.span))
            }
            TokenKind::IntLit(n) => {
                self.bump();
                Ok(SwiftExpr::Int(n, tok.span))
            }
            TokenKind::FloatLit(f) => {
                self.bump();
                Ok(SwiftExpr::Float(f, tok.span))
            }
            TokenKind::BoolLit(b) => {
                self.bump();
                Ok(SwiftExpr::Bool(b, tok.span))
            }
            TokenKind::Nil => {
                self.bump();
                Ok(SwiftExpr::Nil(tok.span))
            }
            TokenKind::ShorthandArg(idx) => {
                self.bump();
                Ok(SwiftExpr::ShorthandArg(idx, tok.span))
            }
            TokenKind::Ident(ident) => {
                self.bump();
                Ok(SwiftExpr::Ident(ident, tok.span))
            }
            TokenKind::Dot => {
                self.bump();
                let (member, _) =
                    self.expect_ident("enum case or member name after leading '.'")?;
                Ok(SwiftExpr::Member {
                    base: None,
                    name: member,
                    span: tok.span,
                })
            }
            TokenKind::LBrace => self.parse_closure_expr(),
            TokenKind::If => self.parse_if_expr(),
            TokenKind::LBracket => {
                self.bump();
                let mut items = Vec::new();
                while self.peek().kind != TokenKind::RBracket && self.peek().kind != TokenKind::Eof
                {
                    items.push(self.parse_expr()?);
                    if self.peek().kind == TokenKind::Comma {
                        self.bump();
                    } else {
                        break;
                    }
                }
                self.expect(&TokenKind::RBracket, "']' to close array literal")?;
                Ok(SwiftExpr::Array(items, tok.span))
            }
            TokenKind::LParen => {
                self.bump();
                let inner = self.parse_expr()?;
                self.expect(&TokenKind::RParen, "')'")?;
                Ok(inner)
            }
            other => Err(ParseError {
                span: tok.span,
                message: format!("unexpected token {other:?} in expression"),
            }),
        }
    }

    fn parse_if_expr(&mut self) -> Result<SwiftExpr, ParseError> {
        let kw = self.bump(); // `if`
        let prev = self.allow_trailing_closure;
        self.allow_trailing_closure = false;
        let cond = self.parse_expr()?;
        self.allow_trailing_closure = prev;

        let then_branch = self.parse_block_stmts()?;
        let else_branch = if self.peek().kind == TokenKind::Else {
            self.bump();
            if self.peek().kind == TokenKind::If {
                vec![SwiftStmt::Expr(self.parse_if_expr()?)]
            } else {
                self.parse_block_stmts()?
            }
        } else {
            Vec::new()
        };
        Ok(SwiftExpr::If {
            cond: Box::new(cond),
            then_branch,
            else_branch,
            span: kw.span,
        })
    }

    fn parse_closure_expr(&mut self) -> Result<SwiftExpr, ParseError> {
        let open = self.expect(&TokenKind::LBrace, "'{' starting closure")?;
        let saved_pos = self.pos;
        let (params, return_type) = match self.try_parse_closure_header() {
            Some(header) => header,
            None => {
                self.pos = saved_pos;
                (Vec::new(), None)
            }
        };

        let mut body = Vec::new();
        self.skip_semicolons();
        while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::Eof {
            body.push(self.parse_stmt()?);
            self.skip_semicolons();
        }
        self.expect(&TokenKind::RBrace, "'}' closing closure")?;

        let max_shorthand = max_shorthand_in_stmts(&body);
        let (uses_shorthand_args, max_shorthand_arity, synthesized_params) =
            if params.is_empty() && max_shorthand.is_some() {
                let arity = max_shorthand.unwrap() + 1;
                let syn = (0..arity)
                    .map(|i| ClosureParam {
                        name: format!("${i}"),
                        ty: None,
                    })
                    .collect();
                (true, arity, syn)
            } else {
                (false, 0, params)
            };

        Ok(SwiftExpr::Closure {
            params: synthesized_params,
            return_type,
            body,
            uses_shorthand_args,
            max_shorthand_arity,
            span: open,
        })
    }

    fn try_parse_closure_header(&mut self) -> Option<(Vec<ClosureParam>, Option<String>)> {
        let mut params = Vec::new();
        if self.peek().kind == TokenKind::LParen {
            self.bump();
            while self.peek().kind != TokenKind::RParen && self.peek().kind != TokenKind::Eof {
                let TokenKind::Ident(name) = self.bump().kind else {
                    return None;
                };
                let ty = if self.peek().kind == TokenKind::Colon {
                    self.bump();
                    self.parse_type_annotation().ok()?
                } else {
                    String::new()
                };
                params.push(ClosureParam {
                    name,
                    ty: (!ty.is_empty()).then_some(ty),
                });
                if self.peek().kind == TokenKind::Comma {
                    self.bump();
                } else {
                    break;
                }
            }
            if self.bump().kind != TokenKind::RParen {
                return None;
            }
        } else {
            loop {
                let TokenKind::Ident(name) = self.peek().kind.clone() else {
                    break;
                };
                self.bump();
                let ty = if self.peek().kind == TokenKind::Colon {
                    self.bump();
                    Some(self.parse_type_annotation().ok()?)
                } else {
                    None
                };
                params.push(ClosureParam { name, ty });
                if self.peek().kind == TokenKind::Comma {
                    self.bump();
                } else {
                    break;
                }
            }
        }

        let return_type = if self.peek().kind == TokenKind::Arrow {
            self.bump();
            Some(self.parse_type_annotation().ok()?)
        } else {
            None
        };

        if self.peek().kind == TokenKind::In {
            self.bump();
            Some((params, return_type))
        } else {
            None
        }
    }

    fn parse_call_args(&mut self) -> Result<Vec<CallArg>, ParseError> {
        let mut args = Vec::new();
        while self.peek().kind != TokenKind::RParen && self.peek().kind != TokenKind::Eof {
            let arg_span = self.peek().span;
            let is_labeled = matches!(
                (&self.peek().kind, &self.peek_n(1).kind),
                (
                    TokenKind::Ident(_)
                        | TokenKind::Import
                        | TokenKind::Let
                        | TokenKind::Var
                        | TokenKind::Func
                        | TokenKind::Interface
                        | TokenKind::Struct
                        | TokenKind::In,
                    TokenKind::Colon
                )
            );
            let label = if is_labeled {
                let (lbl, _) = self.expect_ident("argument label")?;
                self.bump(); // consume ':'
                Some(lbl)
            } else {
                None
            };
            let value = self.parse_expr()?;
            args.push(CallArg {
                label,
                value,
                span: arg_span,
            });
            if self.peek().kind == TokenKind::Comma {
                self.bump();
            } else {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "')' to close argument list")?;
        Ok(args)
    }
}

fn max_shorthand_in_stmts(stmts: &[SwiftStmt]) -> Option<usize> {
    stmts
        .iter()
        .filter_map(|stmt| match stmt {
            SwiftStmt::Binding { value, .. } => max_shorthand_in_expr(value),
            SwiftStmt::Return { value: Some(v), .. } => max_shorthand_in_expr(v),
            SwiftStmt::Expr(e) => max_shorthand_in_expr(e),
            _ => None,
        })
        .max()
}

fn max_shorthand_in_expr(expr: &SwiftExpr) -> Option<usize> {
    match expr {
        SwiftExpr::ShorthandArg(idx, _) => Some(*idx),
        SwiftExpr::Array(items, _) => items.iter().filter_map(max_shorthand_in_expr).max(),
        SwiftExpr::Member { base, .. } => base.as_deref().and_then(max_shorthand_in_expr),
        SwiftExpr::Call { callee, args, .. } => {
            let a = max_shorthand_in_expr(callee);
            let b = args
                .iter()
                .filter_map(|arg| max_shorthand_in_expr(&arg.value))
                .max();
            a.max(b)
        }
        SwiftExpr::Binary { left, right, .. } => {
            max_shorthand_in_expr(left).max(max_shorthand_in_expr(right))
        }
        SwiftExpr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => max_shorthand_in_expr(cond)
            .max(max_shorthand_in_stmts(then_branch))
            .max(max_shorthand_in_stmts(else_branch)),
        SwiftExpr::Closure { .. } => None,
        _ => None,
    }
}

#[derive(Clone, Default)]
struct EvalEnv {
    vars: BTreeMap<String, SwiftExpr>,
    funcs: BTreeMap<String, (Vec<FuncParam>, Vec<SwiftStmt>)>,
    interfaces: BTreeMap<String, (Vec<InterfaceProperty>, Vec<InterfaceMethod>)>,
    structs: BTreeMap<
        String,
        (
            Vec<String>,
            Vec<StructField>,
            BTreeMap<String, (Vec<FuncParam>, Vec<SwiftStmt>)>,
        ),
    >,
}

impl EvalEnv {
    fn with_builtin_protocols() -> Self {
        let mut env = Self::default();
        // Built-in `Package` protocol/interface so any `struct Foo: Package` is recognized by the runtime
        // even without an explicit `interface Package { ... }` declaration in the user file.
        env.interfaces
            .insert("Package".to_string(), (Vec::new(), Vec::new()));
        env
    }
}

fn evaluate_all_packages(
    stmts: &[SwiftStmt],
    tools_version: Option<String>,
) -> Result<Vec<PackageManifest>, ParseError> {
    let mut env = EvalEnv::with_builtin_protocols();
    let mut packages: Vec<PackageManifest> = Vec::new();

    for stmt in stmts {
        match stmt {
            SwiftStmt::Import { .. } => {}
            SwiftStmt::InterfaceDecl {
                name,
                properties,
                methods,
                ..
            } => {
                env.interfaces
                    .insert(name.clone(), (properties.clone(), methods.clone()));
            }
            SwiftStmt::StructDecl {
                name,
                conformances,
                fields,
                methods,
                span,
            } => {
                let mut method_map = BTreeMap::new();
                for m in methods {
                    if let SwiftStmt::FuncDecl {
                        name: m_name,
                        params,
                        body,
                        ..
                    } = m
                    {
                        method_map.insert(m_name.clone(), (params.clone(), body.clone()));
                    }
                }
                let field_names: std::collections::BTreeSet<&str> =
                    fields.iter().map(|f| f.name.as_str()).collect();

                for iface_name in conformances {
                    let (iface_props, iface_methods) =
                        env.interfaces.get(iface_name).ok_or_else(|| ParseError {
                            span: *span,
                            message: format!(
                                "struct '{name}' conforms to unknown interface '{iface_name}'"
                            ),
                        })?;
                    for prop in iface_props {
                        if !field_names.contains(prop.name.as_str())
                            && !method_map.contains_key(&prop.name)
                        {
                            return Err(ParseError {
                                span: *span,
                                message: format!(
                                    "struct '{name}' does not implement required property '{}' of interface '{iface_name}'",
                                    prop.name
                                ),
                            });
                        }
                    }
                    for req in iface_methods {
                        if !method_map.contains_key(&req.name)
                            && !field_names.contains(req.name.as_str())
                        {
                            if let Some(default_body) = &req.default_body {
                                method_map.insert(
                                    req.name.clone(),
                                    (req.params.clone(), default_body.clone()),
                                );
                            } else {
                                return Err(ParseError {
                                    span: *span,
                                    message: format!(
                                        "struct '{name}' does not implement required method '{}' of interface '{iface_name}'",
                                        req.name
                                    ),
                                });
                            }
                        }
                    }
                }
                env.structs.insert(
                    name.clone(),
                    (conformances.clone(), fields.clone(), method_map.clone()),
                );

                // Runtime Package Protocol Conformance Detection:
                // Any `struct <Name>: Package` is automatically synthesized into a `PackageManifest`!
                if conformances.iter().any(|c| c == "Package") {
                    let pkg = eval_package_conforming_struct(
                        name,
                        fields,
                        &method_map,
                        &env,
                        tools_version.clone(),
                        *span,
                    )?;
                    packages.push(pkg);
                }
            }
            SwiftStmt::FuncDecl {
                name, params, body, ..
            } => {
                env.funcs
                    .insert(name.clone(), (params.clone(), body.clone()));
            }
            SwiftStmt::Binding { name, value, .. } => {
                let resolved = resolve_expr(value, &env)?;
                if name == "package" && is_package_call(&resolved) {
                    packages.push(eval_package_call(&resolved, tools_version.clone())?);
                }
                env.vars.insert(name.clone(), resolved);
            }
            SwiftStmt::Expr(expr) => {
                let resolved = resolve_expr(expr, &env)?;
                if is_package_call(&resolved) {
                    packages.push(eval_package_call(&resolved, tools_version.clone())?);
                }
            }
            SwiftStmt::Return { .. } => {}
        }
    }

    Ok(packages)
}

/// Evaluates a `struct <Name>: Package { ... }` conforming to the `Package` protocol/interface
/// into a [`PackageManifest`].
fn eval_package_conforming_struct(
    struct_name: &str,
    fields: &[StructField],
    methods: &BTreeMap<String, (Vec<FuncParam>, Vec<SwiftStmt>)>,
    env: &EvalEnv,
    tools_version: Option<String>,
    span: Span,
) -> Result<PackageManifest, ParseError> {
    let mut local_env = env.clone();
    let mut synth_args = Vec::new();
    let mut has_explicit_name = false;

    for field in fields {
        let Some(default_expr) = &field.default_value else {
            return Err(ParseError {
                span: field.span,
                message: format!(
                    "property '{}' in `struct {struct_name}: Package` must have a value (`= ...` or `{{ ... }}`)",
                    field.name
                ),
            });
        };
        let resolved = resolve_expr(default_expr, &local_env)?;
        local_env.vars.insert(field.name.clone(), resolved.clone());
        if field.name == "name" {
            has_explicit_name = true;
        }
        // Only pass recognized Package properties into the Package descriptor
        if matches!(
            field.name.as_str(),
            "name"
                | "defaultLocalization"
                | "platforms"
                | "products"
                | "dependencies"
                | "targets"
                | "cLanguageStandard"
                | "cxxLanguageStandard"
        ) {
            synth_args.push(CallArg {
                label: Some(field.name.clone()),
                value: resolved,
                span: field.span,
            });
        }
    }

    // Also allow methods like `func targets() -> [Target] { ... }` inside `struct <Name>: Package`
    for (m_name, (params, body)) in methods {
        if params.is_empty()
            && matches!(
                m_name.as_str(),
                "name"
                    | "defaultLocalization"
                    | "platforms"
                    | "products"
                    | "dependencies"
                    | "targets"
                    | "cLanguageStandard"
                    | "cxxLanguageStandard"
            )
            && !synth_args
                .iter()
                .any(|a| a.label.as_deref() == Some(m_name.as_str()))
        {
            let mut fn_env = local_env.clone();
            let resolved = eval_block(body, &mut fn_env)?;
            if m_name == "name" {
                has_explicit_name = true;
            }
            synth_args.push(CallArg {
                label: Some(m_name.clone()),
                value: resolved,
                span,
            });
        }
    }

    if !has_explicit_name {
        synth_args.insert(
            0,
            CallArg {
                label: Some("name".to_string()),
                value: SwiftExpr::String(struct_name.to_string(), span),
                span,
            },
        );
    }

    let synth_call = SwiftExpr::Call {
        callee: Box::new(SwiftExpr::Ident("Package".to_string(), span)),
        args: synth_args,
        span,
    };
    let mut manifest = eval_package_call(&synth_call, tools_version)?;
    manifest.origin = PackageOrigin::ProtocolConformance {
        type_name: struct_name.to_string(),
    };
    Ok(manifest)
}

fn eval_block(stmts: &[SwiftStmt], env: &mut EvalEnv) -> Result<SwiftExpr, ParseError> {
    let mut last = SwiftExpr::Nil(Span { line: 1, column: 1 });
    for stmt in stmts {
        match stmt {
            SwiftStmt::Binding { name, value, .. } => {
                let val = resolve_expr(value, env)?;
                env.vars.insert(name.clone(), val);
            }
            SwiftStmt::Return { value, span } => {
                return match value {
                    Some(v) => resolve_expr(v, env),
                    None => Ok(SwiftExpr::Nil(*span)),
                };
            }
            SwiftStmt::Expr(e) => {
                last = resolve_expr(e, env)?;
            }
            _ => {}
        }
    }
    Ok(last)
}

fn apply_closure(
    closure: &SwiftExpr,
    args: &[SwiftExpr],
    env: &EvalEnv,
) -> Result<SwiftExpr, ParseError> {
    let SwiftExpr::Closure { params, body, .. } = closure else {
        return Err(ParseError {
            span: closure.span(),
            message: "expected closure expression".to_string(),
        });
    };
    let mut local_env = env.clone();
    for (idx, arg_val) in args.iter().enumerate() {
        local_env.vars.insert(format!("${idx}"), arg_val.clone());
        if let Some(param) = params.get(idx) {
            local_env.vars.insert(param.name.clone(), arg_val.clone());
        }
    }
    eval_block(body, &mut local_env)
}

fn resolve_expr(expr: &SwiftExpr, env: &EvalEnv) -> Result<SwiftExpr, ParseError> {
    match expr {
        SwiftExpr::Ident(name, span) => env.vars.get(name).cloned().ok_or_else(|| ParseError {
            span: *span,
            message: format!("undefined identifier '{name}'"),
        }),
        SwiftExpr::ShorthandArg(idx, span) => {
            let key = format!("${idx}");
            env.vars.get(&key).cloned().ok_or_else(|| ParseError {
                span: *span,
                message: format!("unbound shorthand closure argument '${idx}'"),
            })
        }
        SwiftExpr::Array(items, span) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(resolve_expr(item, env)?);
            }
            Ok(SwiftExpr::Array(out, *span))
        }
        SwiftExpr::Member {
            base: Some(base_expr),
            name,
            span,
        } => {
            let resolved_base = resolve_expr(base_expr, env)?;
            if let SwiftExpr::Call {
                callee,
                args: struct_fields,
                ..
            } = &resolved_base
            {
                if matches!(callee.as_ref(), SwiftExpr::Ident(s, _) if s.starts_with("__struct_instance:"))
                {
                    for f in struct_fields {
                        if f.label.as_deref() == Some(name.as_str()) {
                            return Ok(f.value.clone());
                        }
                    }
                }
            }
            Ok(SwiftExpr::Member {
                base: Some(Box::new(resolved_base)),
                name: name.clone(),
                span: *span,
            })
        }
        SwiftExpr::If {
            cond,
            then_branch,
            else_branch,
            span,
        } => {
            let c = resolve_expr(cond, env)?;
            let SwiftExpr::Bool(b, _) = c else {
                return Err(ParseError {
                    span: *span,
                    message: "if condition must evaluate to Bool".to_string(),
                });
            };
            let mut branch_env = env.clone();
            if b {
                eval_block(then_branch, &mut branch_env)
            } else {
                eval_block(else_branch, &mut branch_env)
            }
        }
        SwiftExpr::Call { callee, args, span } => {
            if let SwiftExpr::Member {
                base: Some(receiver),
                name: method_name,
                ..
            } = callee.as_ref()
            {
                let resolved_recv = resolve_expr(receiver, env)?;
                if let SwiftExpr::Array(items, arr_span) = &resolved_recv {
                    match method_name.as_str() {
                        "map" if args.len() == 1 => {
                            let closure = resolve_expr(&args[0].value, env)?;
                            let mut mapped = Vec::with_capacity(items.len());
                            for item in items {
                                mapped.push(apply_closure(
                                    &closure,
                                    std::slice::from_ref(item),
                                    env,
                                )?);
                            }
                            return Ok(SwiftExpr::Array(mapped, *arr_span));
                        }
                        "filter" if args.len() == 1 => {
                            let closure = resolve_expr(&args[0].value, env)?;
                            let mut filtered = Vec::new();
                            for item in items {
                                let keep =
                                    apply_closure(&closure, std::slice::from_ref(item), env)?;
                                let SwiftExpr::Bool(b, _) = keep else {
                                    return Err(ParseError {
                                        span: *span,
                                        message: "`.filter` closure must return Bool".to_string(),
                                    });
                                };
                                if b {
                                    filtered.push(item.clone());
                                }
                            }
                            return Ok(SwiftExpr::Array(filtered, *arr_span));
                        }
                        "reduce" if args.len() == 2 => {
                            let mut acc = resolve_expr(&args[0].value, env)?;
                            let closure = resolve_expr(&args[1].value, env)?;
                            for item in items {
                                acc = apply_closure(&closure, &[acc, item.clone()], env)?;
                            }
                            return Ok(acc);
                        }
                        _ => {}
                    }
                }

                if let SwiftExpr::Call {
                    callee: inst_callee,
                    args: inst_fields,
                    ..
                } = &resolved_recv
                {
                    if let SwiftExpr::Ident(tag, _) = inst_callee.as_ref() {
                        if let Some(struct_name) = tag.strip_prefix("__struct_instance:") {
                            if let Some((_, _, methods)) = env.structs.get(struct_name) {
                                if let Some((params, body)) = methods.get(method_name) {
                                    let mut fn_env = env.clone();
                                    fn_env
                                        .vars
                                        .insert("self".to_string(), resolved_recv.clone());
                                    for f in inst_fields {
                                        if let Some(lbl) = &f.label {
                                            fn_env.vars.insert(lbl.clone(), f.value.clone());
                                        }
                                    }
                                    for (param, call_arg) in params.iter().zip(args.iter()) {
                                        let val = resolve_expr(&call_arg.value, env)?;
                                        fn_env.vars.insert(param.name.clone(), val);
                                    }
                                    return eval_block(body, &mut fn_env);
                                }
                            }
                        }
                    }
                }
            }

            if let SwiftExpr::Ident(fn_name, _) = callee.as_ref() {
                if let Some((params, body)) = env.funcs.get(fn_name) {
                    let mut fn_env = env.clone();
                    for (param, call_arg) in params.iter().zip(args.iter()) {
                        let val = resolve_expr(&call_arg.value, env)?;
                        fn_env.vars.insert(param.name.clone(), val);
                    }
                    return eval_block(body, &mut fn_env);
                }
                if let Some((_, declared_fields, _)) = env.structs.get(fn_name) {
                    let mut resolved_fields = Vec::new();
                    for df in declared_fields {
                        if let Some(explicit) = args
                            .iter()
                            .find(|a| a.label.as_deref() == Some(df.name.as_str()))
                        {
                            resolved_fields.push(CallArg {
                                label: Some(df.name.clone()),
                                value: resolve_expr(&explicit.value, env)?,
                                span: explicit.span,
                            });
                        } else if let Some(def_val) = &df.default_value {
                            resolved_fields.push(CallArg {
                                label: Some(df.name.clone()),
                                value: resolve_expr(def_val, env)?,
                                span: df.span,
                            });
                        }
                    }
                    for arg in args {
                        if !resolved_fields
                            .iter()
                            .any(|rf| rf.label == arg.label && arg.label.is_some())
                        {
                            resolved_fields.push(CallArg {
                                label: arg.label.clone(),
                                value: resolve_expr(&arg.value, env)?,
                                span: arg.span,
                            });
                        }
                    }
                    return Ok(SwiftExpr::Call {
                        callee: Box::new(SwiftExpr::Ident(
                            format!("__struct_instance:{fn_name}"),
                            *span,
                        )),
                        args: resolved_fields,
                        span: *span,
                    });
                }
                if let Some(closure_val) = env.vars.get(fn_name) {
                    if matches!(closure_val, SwiftExpr::Closure { .. }) {
                        let mut eval_args = Vec::with_capacity(args.len());
                        for a in args {
                            eval_args.push(resolve_expr(&a.value, env)?);
                        }
                        return apply_closure(closure_val, &eval_args, env);
                    }
                }
            }

            let mut resolved_args = Vec::with_capacity(args.len());
            for arg in args {
                resolved_args.push(CallArg {
                    label: arg.label.clone(),
                    value: resolve_expr(&arg.value, env)?,
                    span: arg.span,
                });
            }
            Ok(SwiftExpr::Call {
                callee: callee.clone(),
                args: resolved_args,
                span: *span,
            })
        }
        SwiftExpr::Binary {
            op,
            left,
            right,
            span,
        } => {
            let l = resolve_expr(left, env)?;
            let r = resolve_expr(right, env)?;
            match (op, l, r) {
                (BinOp::Add, SwiftExpr::String(mut a, s), SwiftExpr::String(b, _)) => {
                    a.push_str(&b);
                    Ok(SwiftExpr::String(a, s))
                }
                (BinOp::Add, SwiftExpr::Array(mut a, s), SwiftExpr::Array(b, _)) => {
                    a.extend(b);
                    Ok(SwiftExpr::Array(a, s))
                }
                (BinOp::Add, SwiftExpr::Int(a, s), SwiftExpr::Int(b, _)) => {
                    Ok(SwiftExpr::Int(a + b, s))
                }
                (BinOp::Sub, SwiftExpr::Int(a, s), SwiftExpr::Int(b, _)) => {
                    Ok(SwiftExpr::Int(a - b, s))
                }
                (BinOp::Mul, SwiftExpr::Int(a, s), SwiftExpr::Int(b, _)) => {
                    Ok(SwiftExpr::Int(a * b, s))
                }
                (BinOp::Div, SwiftExpr::Int(a, s), SwiftExpr::Int(b, _)) => {
                    Ok(SwiftExpr::Int(a / b, s))
                }
                (BinOp::Eq, SwiftExpr::Int(a, s), SwiftExpr::Int(b, _)) => {
                    Ok(SwiftExpr::Bool(a == b, s))
                }
                (BinOp::NotEq, SwiftExpr::Int(a, s), SwiftExpr::Int(b, _)) => {
                    Ok(SwiftExpr::Bool(a != b, s))
                }
                (BinOp::Lt, SwiftExpr::Int(a, s), SwiftExpr::Int(b, _)) => {
                    Ok(SwiftExpr::Bool(a < b, s))
                }
                (BinOp::Le, SwiftExpr::Int(a, s), SwiftExpr::Int(b, _)) => {
                    Ok(SwiftExpr::Bool(a <= b, s))
                }
                (BinOp::Gt, SwiftExpr::Int(a, s), SwiftExpr::Int(b, _)) => {
                    Ok(SwiftExpr::Bool(a > b, s))
                }
                (BinOp::Ge, SwiftExpr::Int(a, s), SwiftExpr::Int(b, _)) => {
                    Ok(SwiftExpr::Bool(a >= b, s))
                }
                (BinOp::Eq, SwiftExpr::String(a, s), SwiftExpr::String(b, _)) => {
                    Ok(SwiftExpr::Bool(a == b, s))
                }
                (BinOp::NotEq, SwiftExpr::String(a, s), SwiftExpr::String(b, _)) => {
                    Ok(SwiftExpr::Bool(a != b, s))
                }
                _ => Err(ParseError {
                    span: *span,
                    message: format!("unsupported operands for binary operator {op:?}"),
                }),
            }
        }
        other => Ok(other.clone()),
    }
}

// ============================================================================
// LLVM IR Code Generation Backend (`emit_llvm_ir`)
// ============================================================================

/// Compiles a Swift source program (containing `interface`, `struct`, `func`, closures `{ $0 }`,
/// arithmetic, and control flow) into valid textual LLVM IR (`.ll`).
pub fn emit_llvm_ir(source: &str, module_name: &str) -> Result<String, ParseError> {
    let stmts = parse_swift_ast(source)?;
    let mut codegen = LlvmCodegen::new(module_name);
    codegen.compile_module(&stmts)?;
    Ok(codegen.finish())
}

struct LlvmCodegen {
    module_name: String,
    type_defs: String,
    globals: String,
    functions: String,
    closure_counter: usize,
    reg_counter: usize,
    label_counter: usize,
    interfaces: BTreeMap<String, Vec<InterfaceMethod>>,
    structs: BTreeMap<String, (Vec<String>, Vec<StructField>, Vec<SwiftStmt>)>,
}

impl LlvmCodegen {
    fn new(module_name: &str) -> Self {
        Self {
            module_name: module_name.to_string(),
            type_defs: String::new(),
            globals: String::new(),
            functions: String::new(),
            closure_counter: 0,
            reg_counter: 0,
            label_counter: 0,
            interfaces: BTreeMap::new(),
            structs: BTreeMap::new(),
        }
    }

    fn next_reg(&mut self) -> String {
        self.reg_counter += 1;
        format!("%r{}", self.reg_counter)
    }

    fn next_label(&mut self, prefix: &str) -> String {
        self.label_counter += 1;
        format!("{prefix}_{}", self.label_counter)
    }

    fn llvm_type(&self, swift_ty: Option<&str>) -> String {
        match swift_ty.unwrap_or("Int") {
            "Int" | "Int64" => "i64".to_string(),
            "Bool" => "i1".to_string(),
            "Double" | "Float" => "double".to_string(),
            "Void" | "()" => "void".to_string(),
            "String" => "ptr".to_string(),
            other if other.starts_with('[') => "ptr".to_string(),
            other if self.interfaces.contains_key(other) => {
                format!("%interface.{other}.FatPtr")
            }
            other if self.structs.contains_key(other) => {
                format!("%struct.{other}")
            }
            _ => "i64".to_string(),
        }
    }

    fn compile_module(&mut self, stmts: &[SwiftStmt]) -> Result<(), ParseError> {
        self.interfaces.insert("Package".to_string(), Vec::new());

        for stmt in stmts {
            match stmt {
                SwiftStmt::InterfaceDecl { name, methods, .. } => {
                    self.interfaces.insert(name.clone(), methods.clone());
                }
                SwiftStmt::StructDecl {
                    name,
                    conformances,
                    fields,
                    methods,
                    ..
                } => {
                    self.structs.insert(
                        name.clone(),
                        (conformances.clone(), fields.clone(), methods.clone()),
                    );
                }
                _ => {}
            }
        }

        let interfaces_snapshot = self.interfaces.clone();
        for (iface_name, methods) in &interfaces_snapshot {
            let vtable_slots: Vec<String> = if methods.is_empty() {
                vec!["ptr".to_string()]
            } else {
                methods.iter().map(|_| "ptr".to_string()).collect()
            };
            let _ = writeln!(
                self.type_defs,
                "%interface.{iface_name}.VTable = type {{ {} }}",
                vtable_slots.join(", ")
            );
            let _ = writeln!(
                self.type_defs,
                "%interface.{iface_name}.FatPtr = type {{ ptr, ptr }}"
            );

            for (slot_idx, m) in methods.iter().enumerate() {
                let ret_ty = self.llvm_type(m.return_type.as_deref());
                let mut sig_params = vec![format!("%interface.{iface_name}.FatPtr %iface_ref")];
                let mut call_args = vec!["ptr %data_ptr".to_string()];
                for p in &m.params {
                    let p_ty = self.llvm_type(Some(&p.ty));
                    sig_params.push(format!("{p_ty} %{}", p.name));
                    call_args.push(format!("{p_ty} %{}", p.name));
                }

                let _ = writeln!(
                    self.functions,
                    "define {ret_ty} @_dispatch_{iface_name}_{}({}) {{",
                    m.name,
                    sig_params.join(", ")
                );
                let _ = writeln!(self.functions, "entry:");
                let _ = writeln!(
                    self.functions,
                    "  %data_ptr = extractvalue %interface.{iface_name}.FatPtr %iface_ref, 0"
                );
                let _ = writeln!(
                    self.functions,
                    "  %vtable_ptr = extractvalue %interface.{iface_name}.FatPtr %iface_ref, 1"
                );
                let _ = writeln!(
                    self.functions,
                    "  %slot_ptr = getelementptr inbounds %interface.{iface_name}.VTable, ptr %vtable_ptr, i32 0, i32 {slot_idx}"
                );
                let _ = writeln!(
                    self.functions,
                    "  %fn_ptr = load ptr, ptr %slot_ptr, align 8"
                );
                if ret_ty == "void" {
                    let _ = writeln!(
                        self.functions,
                        "  call void %fn_ptr({})",
                        call_args.join(", ")
                    );
                    let _ = writeln!(self.functions, "  ret void");
                } else {
                    let _ = writeln!(
                        self.functions,
                        "  %res = call {ret_ty} %fn_ptr({})",
                        call_args.join(", ")
                    );
                    let _ = writeln!(self.functions, "  ret {ret_ty} %res");
                }
                let _ = writeln!(self.functions, "}}\n");
            }
        }

        let structs_snapshot = self.structs.clone();
        for (struct_name, (conformances, fields, methods)) in &structs_snapshot {
            let field_tys: Vec<String> = if fields.is_empty() {
                vec!["i8".to_string()]
            } else {
                fields
                    .iter()
                    .map(|f| self.llvm_type(f.ty.as_deref()))
                    .collect()
            };
            let _ = writeln!(
                self.type_defs,
                "%struct.{struct_name} = type {{ {} }}",
                field_tys.join(", ")
            );

            let mut compiled_method_names = BTreeMap::new();
            for m_stmt in methods {
                if let SwiftStmt::FuncDecl {
                    name: m_name,
                    params,
                    return_type,
                    body,
                    ..
                } = m_stmt
                {
                    let sym = format!("{struct_name}_{m_name}");
                    self.compile_function(
                        &sym,
                        Some((struct_name.as_str(), fields.as_slice())),
                        params,
                        return_type.as_deref(),
                        body,
                    )?;
                    compiled_method_names.insert(m_name.clone(), format!("@{sym}"));
                }
            }

            for iface_name in conformances {
                if let Some(iface_methods) = interfaces_snapshot.get(iface_name) {
                    let mut slot_ptrs = Vec::new();
                    for req in iface_methods {
                        if let Some(fn_sym) = compiled_method_names.get(&req.name) {
                            slot_ptrs.push(format!("ptr {fn_sym}"));
                        } else if let Some(default_body) = &req.default_body {
                            let sym = format!("{struct_name}_{}", req.name);
                            self.compile_function(
                                &sym,
                                Some((struct_name.as_str(), fields.as_slice())),
                                &req.params,
                                req.return_type.as_deref(),
                                default_body,
                            )?;
                            slot_ptrs.push(format!("ptr @{sym}"));
                        }
                    }
                    if slot_ptrs.is_empty() {
                        slot_ptrs.push("ptr null".to_string());
                    }
                    let _ = writeln!(
                        self.globals,
                        "@_vtable_{struct_name}_as_{iface_name} = private unnamed_addr constant %interface.{iface_name}.VTable {{ {} }}",
                        slot_ptrs.join(", ")
                    );
                    let _ = writeln!(
                        self.functions,
                        "define %interface.{iface_name}.FatPtr @_upcast_{struct_name}_to_{iface_name}(ptr %self_ptr) {{"
                    );
                    let _ = writeln!(self.functions, "entry:");
                    let _ = writeln!(
                        self.functions,
                        "  %fat0 = insertvalue %interface.{iface_name}.FatPtr undef, ptr %self_ptr, 0"
                    );
                    let _ = writeln!(
                        self.functions,
                        "  %fat1 = insertvalue %interface.{iface_name}.FatPtr %fat0, ptr @_vtable_{struct_name}_as_{iface_name}, 1"
                    );
                    let _ = writeln!(self.functions, "  ret %interface.{iface_name}.FatPtr %fat1");
                    let _ = writeln!(self.functions, "}}\n");
                }
            }
        }

        let mut top_level_body = Vec::new();
        for stmt in stmts {
            match stmt {
                SwiftStmt::FuncDecl {
                    name,
                    params,
                    return_type,
                    body,
                    ..
                } => {
                    self.compile_function(name, None, params, return_type.as_deref(), body)?;
                }
                SwiftStmt::Binding { .. } | SwiftStmt::Expr(_) | SwiftStmt::Return { .. } => {
                    top_level_body.push(stmt.clone());
                }
                _ => {}
            }
        }

        if !top_level_body.is_empty() {
            self.compile_top_level_entry(&top_level_body)?;
        }

        Ok(())
    }

    fn compile_function(
        &mut self,
        fn_name: &str,
        self_struct: Option<(&str, &[StructField])>,
        params: &[FuncParam],
        return_type: Option<&str>,
        body: &[SwiftStmt],
    ) -> Result<(), ParseError> {
        let ret_ty = self.llvm_type(return_type);
        let mut sig_params = Vec::new();
        let mut local_vars: BTreeMap<String, String> = BTreeMap::new();
        let mut struct_fields_map: BTreeMap<String, usize> = BTreeMap::new();
        let mut self_struct_name = None;

        if let Some((s_name, fields)) = self_struct {
            sig_params.push("ptr %self".to_string());
            self_struct_name = Some(s_name.to_string());
            for (idx, f) in fields.iter().enumerate() {
                struct_fields_map.insert(f.name.clone(), idx);
            }
        }
        for p in params {
            let p_ty = self.llvm_type(Some(&p.ty));
            let reg = format!("%{}", p.name);
            sig_params.push(format!("{p_ty} {reg}"));
            local_vars.insert(p.name.clone(), reg);
        }

        let mut fn_buf = String::new();
        let _ = writeln!(
            fn_buf,
            "define {ret_ty} @{fn_name}({}) {{",
            sig_params.join(", ")
        );
        let _ = writeln!(fn_buf, "entry:");

        let mut last_val = "0".to_string();
        let mut has_explicit_ret = false;
        for stmt in body {
            match stmt {
                SwiftStmt::Binding { name, value, .. } => {
                    let val_reg = self.compile_expr(
                        value,
                        &mut fn_buf,
                        &mut local_vars,
                        self_struct_name.as_deref(),
                        &struct_fields_map,
                    )?;
                    local_vars.insert(name.clone(), val_reg);
                }
                SwiftStmt::Return { value, .. } => {
                    has_explicit_ret = true;
                    if let Some(v) = value {
                        let val_reg = self.compile_expr(
                            v,
                            &mut fn_buf,
                            &mut local_vars,
                            self_struct_name.as_deref(),
                            &struct_fields_map,
                        )?;
                        let _ = writeln!(fn_buf, "  ret {ret_ty} {val_reg}");
                    } else {
                        let _ = writeln!(fn_buf, "  ret void");
                    }
                    break;
                }
                SwiftStmt::Expr(e) => {
                    last_val = self.compile_expr(
                        e,
                        &mut fn_buf,
                        &mut local_vars,
                        self_struct_name.as_deref(),
                        &struct_fields_map,
                    )?;
                }
                _ => {}
            }
        }

        if !has_explicit_ret {
            if ret_ty == "void" {
                let _ = writeln!(fn_buf, "  ret void");
            } else {
                let _ = writeln!(fn_buf, "  ret {ret_ty} {last_val}");
            }
        }
        let _ = writeln!(fn_buf, "}}\n");
        self.functions.push_str(&fn_buf);
        Ok(())
    }

    fn compile_top_level_entry(&mut self, stmts: &[SwiftStmt]) -> Result<(), ParseError> {
        let mut fn_buf = String::new();
        let _ = writeln!(fn_buf, "define i64 @swift_entry() {{");
        let _ = writeln!(fn_buf, "entry:");
        let mut local_vars: BTreeMap<String, String> = BTreeMap::new();
        let empty_fields = BTreeMap::new();
        let mut last_val = "0".to_string();

        for stmt in stmts {
            match stmt {
                SwiftStmt::Binding { name, value, .. } => {
                    if is_package_call(value) {
                        continue;
                    }
                    let val_reg = self.compile_expr(
                        value,
                        &mut fn_buf,
                        &mut local_vars,
                        None,
                        &empty_fields,
                    )?;
                    local_vars.insert(name.clone(), val_reg.clone());
                    last_val = val_reg;
                }
                SwiftStmt::Expr(e) => {
                    if is_package_call(e) {
                        continue;
                    }
                    last_val =
                        self.compile_expr(e, &mut fn_buf, &mut local_vars, None, &empty_fields)?;
                }
                SwiftStmt::Return { value: Some(v), .. } => {
                    last_val =
                        self.compile_expr(v, &mut fn_buf, &mut local_vars, None, &empty_fields)?;
                }
                _ => {}
            }
        }

        let _ = writeln!(fn_buf, "  ret i64 {last_val}");
        let _ = writeln!(fn_buf, "}}\n");
        self.functions.push_str(&fn_buf);
        Ok(())
    }

    fn compile_expr(
        &mut self,
        expr: &SwiftExpr,
        buf: &mut String,
        local_vars: &mut BTreeMap<String, String>,
        self_struct: Option<&str>,
        struct_fields: &BTreeMap<String, usize>,
    ) -> Result<String, ParseError> {
        match expr {
            SwiftExpr::Int(n, _) => Ok(n.to_string()),
            SwiftExpr::Bool(b, _) => Ok(if *b { "1" } else { "0" }.to_string()),
            SwiftExpr::ShorthandArg(idx, _) => {
                let key = format!("${idx}");
                Ok(local_vars
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| format!("%arg{idx}")))
            }
            SwiftExpr::Ident(name, _) => {
                if let Some(reg) = local_vars.get(name) {
                    return Ok(reg.clone());
                }
                if let (Some(s_name), Some(field_idx)) = (self_struct, struct_fields.get(name)) {
                    let ptr_reg = self.next_reg();
                    let val_reg = self.next_reg();
                    let _ = writeln!(
                        buf,
                        "  {ptr_reg} = getelementptr inbounds %struct.{s_name}, ptr %self, i32 0, i32 {field_idx}"
                    );
                    let _ = writeln!(buf, "  {val_reg} = load i64, ptr {ptr_reg}, align 8");
                    return Ok(val_reg);
                }
                Ok("0".to_string())
            }
            SwiftExpr::Member {
                base: Some(base_expr),
                name,
                ..
            } => {
                if matches!(base_expr.as_ref(), SwiftExpr::Ident(id, _) if id == "self") {
                    if let (Some(s_name), Some(field_idx)) = (self_struct, struct_fields.get(name))
                    {
                        let ptr_reg = self.next_reg();
                        let val_reg = self.next_reg();
                        let _ = writeln!(
                            buf,
                            "  {ptr_reg} = getelementptr inbounds %struct.{s_name}, ptr %self, i32 0, i32 {field_idx}"
                        );
                        let _ = writeln!(buf, "  {val_reg} = load i64, ptr {ptr_reg}, align 8");
                        return Ok(val_reg);
                    }
                }
                Ok("0".to_string())
            }
            SwiftExpr::Binary {
                op, left, right, ..
            } => {
                let l = self.compile_expr(left, buf, local_vars, self_struct, struct_fields)?;
                let r = self.compile_expr(right, buf, local_vars, self_struct, struct_fields)?;
                let dst = self.next_reg();
                match op {
                    BinOp::Add => {
                        let _ = writeln!(buf, "  {dst} = add nsw i64 {l}, {r}");
                        Ok(dst)
                    }
                    BinOp::Sub => {
                        let _ = writeln!(buf, "  {dst} = sub nsw i64 {l}, {r}");
                        Ok(dst)
                    }
                    BinOp::Mul => {
                        let _ = writeln!(buf, "  {dst} = mul nsw i64 {l}, {r}");
                        Ok(dst)
                    }
                    BinOp::Div => {
                        let _ = writeln!(buf, "  {dst} = sdiv i64 {l}, {r}");
                        Ok(dst)
                    }
                    BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        let pred = match op {
                            BinOp::Eq => "eq",
                            BinOp::NotEq => "ne",
                            BinOp::Lt => "slt",
                            BinOp::Le => "sle",
                            BinOp::Gt => "sgt",
                            BinOp::Ge => "sge",
                            _ => unreachable!(),
                        };
                        let _ = writeln!(buf, "  {dst} = icmp {pred} i64 {l}, {r}");
                        Ok(dst)
                    }
                }
            }
            SwiftExpr::If {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                let c_reg = self.compile_expr(cond, buf, local_vars, self_struct, struct_fields)?;
                let then_lbl = self.next_label("then");
                let else_lbl = self.next_label("else");
                let merge_lbl = self.next_label("merge");

                let _ = writeln!(buf, "  br i1 {c_reg}, label %{then_lbl}, label %{else_lbl}");
                let _ = writeln!(buf, "{then_lbl}:");
                let then_val = if let Some(SwiftStmt::Expr(e)) = then_branch.last() {
                    self.compile_expr(e, buf, local_vars, self_struct, struct_fields)?
                } else {
                    "0".to_string()
                };
                let _ = writeln!(buf, "  br label %{merge_lbl}");

                let _ = writeln!(buf, "{else_lbl}:");
                let else_val = if let Some(SwiftStmt::Expr(e)) = else_branch.last() {
                    self.compile_expr(e, buf, local_vars, self_struct, struct_fields)?
                } else {
                    "0".to_string()
                };
                let _ = writeln!(buf, "  br label %{merge_lbl}");

                let _ = writeln!(buf, "{merge_lbl}:");
                let phi_reg = self.next_reg();
                let _ = writeln!(
                    buf,
                    "  {phi_reg} = phi i64 [ {then_val}, %{then_lbl} ], [ {else_val}, %{else_lbl} ]"
                );
                Ok(phi_reg)
            }
            SwiftExpr::Closure {
                params,
                return_type,
                body,
                ..
            } => {
                self.closure_counter += 1;
                let closure_sym = format!("_closure_{}", self.closure_counter);
                let fn_params: Vec<FuncParam> = if params.is_empty() {
                    vec![FuncParam {
                        external_label: None,
                        name: "arg0".to_string(),
                        ty: "Int".to_string(),
                    }]
                } else {
                    params
                        .iter()
                        .enumerate()
                        .map(|(i, p)| FuncParam {
                            external_label: None,
                            name: if p.name.starts_with('$') {
                                format!("arg{i}")
                            } else {
                                p.name.clone()
                            },
                            ty: p.ty.clone().unwrap_or_else(|| "Int".to_string()),
                        })
                        .collect()
                };
                self.compile_function(
                    &closure_sym,
                    None,
                    &fn_params,
                    return_type.as_deref(),
                    body,
                )?;
                Ok(format!("@{closure_sym}"))
            }
            SwiftExpr::Call { callee, args, .. } => {
                if let SwiftExpr::Ident(fn_name, _) = callee.as_ref() {
                    if self.structs.contains_key(fn_name) {
                        let ptr_reg = self.next_reg();
                        let _ = writeln!(buf, "  {ptr_reg} = alloca %struct.{fn_name}, align 8");
                        for (idx, arg) in args.iter().enumerate() {
                            let val_reg = self.compile_expr(
                                &arg.value,
                                buf,
                                local_vars,
                                self_struct,
                                struct_fields,
                            )?;
                            let field_ptr = self.next_reg();
                            let _ = writeln!(
                                buf,
                                "  {field_ptr} = getelementptr inbounds %struct.{fn_name}, ptr {ptr_reg}, i32 0, i32 {idx}"
                            );
                            let _ =
                                writeln!(buf, "  store i64 {val_reg}, ptr {field_ptr}, align 8");
                        }
                        return Ok(ptr_reg);
                    }

                    let mut compiled_args = Vec::new();
                    for a in args {
                        let v = self.compile_expr(
                            &a.value,
                            buf,
                            local_vars,
                            self_struct,
                            struct_fields,
                        )?;
                        compiled_args.push(format!("i64 {v}"));
                    }
                    let target_sym = local_vars
                        .get(fn_name)
                        .cloned()
                        .unwrap_or_else(|| format!("@{fn_name}"));
                    let dst = self.next_reg();
                    let _ = writeln!(
                        buf,
                        "  {dst} = call i64 {target_sym}({})",
                        compiled_args.join(", ")
                    );
                    return Ok(dst);
                }
                Ok("0".to_string())
            }
            _ => Ok("0".to_string()),
        }
    }

    fn finish(self) -> String {
        let mut ir = String::new();
        let _ = writeln!(ir, "; ModuleID = '{}'", self.module_name);
        let _ = writeln!(ir, "source_filename = \"{}.swift\"", self.module_name);
        if !self.type_defs.is_empty() {
            let _ = writeln!(ir, "\n; --- Struct & Interface VTable Types ---");
            ir.push_str(&self.type_defs);
        }
        if !self.globals.is_empty() {
            let _ = writeln!(ir, "\n; --- Interface VTable Globals ---");
            ir.push_str(&self.globals);
        }
        let _ = writeln!(ir, "\n; --- Compiled Functions, Closures & Dispatchers ---");
        ir.push_str(&self.functions);
        ir
    }
}

fn is_package_call(expr: &SwiftExpr) -> bool {
    matches!(
        expr,
        SwiftExpr::Call { callee, .. }
            if matches!(callee.as_ref(), SwiftExpr::Ident(name, _) if name == "Package")
    )
}

fn eval_package_call(
    expr: &SwiftExpr,
    tools_version: Option<String>,
) -> Result<PackageManifest, ParseError> {
    let SwiftExpr::Call { args, span, .. } = expr else {
        return Err(ParseError {
            span: expr.span(),
            message: "expected Package(...) call".to_string(),
        });
    };

    let mut name: Option<String> = None;
    let mut default_localization: Option<String> = None;
    let mut platforms = Vec::new();
    let mut products = Vec::new();
    let mut dependencies = Vec::new();
    let mut targets = Vec::new();
    let mut c_language_standard = None;
    let mut cxx_language_standard = None;

    for arg in args {
        let label = arg.label.as_deref().ok_or_else(|| ParseError {
            span: arg.span,
            message: "Package(...) arguments must be labeled (e.g. `name: \"...\"`)".to_string(),
        })?;
        match label {
            "name" => name = Some(expect_string(&arg.value, "Package.name")?),
            "defaultLocalization" => {
                default_localization =
                    Some(expect_string(&arg.value, "Package.defaultLocalization")?)
            }
            "platforms" => {
                for item in expect_array(&arg.value, "Package.platforms")? {
                    platforms.push(eval_platform(item)?);
                }
            }
            "products" => {
                for item in expect_array(&arg.value, "Package.products")? {
                    products.push(eval_product(item)?);
                }
            }
            "dependencies" => {
                for item in expect_array(&arg.value, "Package.dependencies")? {
                    dependencies.push(eval_package_dependency(item)?);
                }
            }
            "targets" => {
                for item in expect_array(&arg.value, "Package.targets")? {
                    targets.push(eval_target(item)?);
                }
            }
            "cLanguageStandard" => {
                c_language_standard = Some(expect_enum_or_string(
                    &arg.value,
                    "Package.cLanguageStandard",
                )?)
            }
            "cxxLanguageStandard" => {
                cxx_language_standard = Some(expect_enum_or_string(
                    &arg.value,
                    "Package.cxxLanguageStandard",
                )?)
            }
            other => {
                return Err(ParseError {
                    span: arg.span,
                    message: format!("unknown parameter '{other}' in Package(...)"),
                });
            }
        }
    }

    let pkg_name = name.ok_or_else(|| ParseError {
        span: *span,
        message: "missing required `name:` parameter in Package(...)".to_string(),
    })?;

    Ok(PackageManifest {
        tools_version,
        name: pkg_name,
        origin: PackageOrigin::InitializerCall,
        default_localization,
        platforms,
        products,
        dependencies,
        targets,
        c_language_standard,
        cxx_language_standard,
    })
}

fn eval_platform(expr: &SwiftExpr) -> Result<PlatformSpec, ParseError> {
    match expr {
        SwiftExpr::Member { name, .. } => Ok(PlatformSpec {
            name: name.clone(),
            version_or_board: None,
            options: BTreeMap::new(),
        }),
        SwiftExpr::Call { callee, args, .. } => {
            let platform_name = extract_member_name(callee, "platform specifier")?;
            let mut version_or_board = None;
            let mut options = BTreeMap::new();
            for (idx, arg) in args.iter().enumerate() {
                match arg.label.as_deref() {
                    None if idx == 0 => {
                        version_or_board =
                            Some(expect_enum_or_string(&arg.value, "platform version")?);
                    }
                    Some("board") | Some("version") if version_or_board.is_none() => {
                        version_or_board =
                            Some(expect_enum_or_string(&arg.value, "platform board/version")?);
                    }
                    Some(key) => {
                        options.insert(key.to_string(), expect_enum_or_string(&arg.value, key)?);
                    }
                    None => {
                        return Err(ParseError {
                            span: arg.span,
                            message: "unexpected positional argument in platform specifier"
                                .to_string(),
                        });
                    }
                }
            }
            Ok(PlatformSpec {
                name: platform_name,
                version_or_board,
                options,
            })
        }
        _ => Err(ParseError {
            span: expr.span(),
            message: "expected platform specifier like `.macOS(.v14)` or `.rp2350(board: .pico2W)`"
                .to_string(),
        }),
    }
}

fn eval_product(expr: &SwiftExpr) -> Result<ProductSpec, ParseError> {
    let SwiftExpr::Call { callee, args, span } = expr else {
        return Err(ParseError {
            span: expr.span(),
            message: "expected product constructor like `.library(...)` or `.executable(...)`"
                .to_string(),
        });
    };
    let kind_name = extract_member_name(callee, "product constructor")?;
    let mut name = None;
    let mut linkage = None;
    let mut format = None;
    let mut targets = Vec::new();

    for arg in args {
        match arg.label.as_deref() {
            Some("name") => name = Some(expect_string(&arg.value, "Product.name")?),
            Some("type") => linkage = Some(expect_enum_or_string(&arg.value, "Product.type")?),
            Some("format") => format = Some(expect_enum_or_string(&arg.value, "Product.format")?),
            Some("targets") => {
                for t in expect_array(&arg.value, "Product.targets")? {
                    targets.push(expect_string(t, "Product.targets element")?);
                }
            }
            other => {
                return Err(ParseError {
                    span: arg.span,
                    message: format!("unknown argument '{other:?}' in .{kind_name}(...)"),
                });
            }
        }
    }

    let prod_name = name.ok_or_else(|| ParseError {
        span: *span,
        message: format!("missing `name:` in .{kind_name}(...)"),
    })?;

    let kind = match kind_name.as_str() {
        "library" => ProductKind::Library { linkage },
        "executable" => ProductKind::Executable,
        "firmware" => ProductKind::Firmware { format },
        "plugin" => ProductKind::Plugin,
        other => {
            return Err(ParseError {
                span: *span,
                message: format!("unsupported product kind '.{other}'"),
            });
        }
    };

    Ok(ProductSpec {
        name: prod_name,
        kind,
        targets,
    })
}

fn eval_package_dependency(expr: &SwiftExpr) -> Result<PackageDependency, ParseError> {
    let SwiftExpr::Call { callee, args, span } = expr else {
        return Err(ParseError {
            span: expr.span(),
            message: "expected `.package(...)` in dependencies".to_string(),
        });
    };
    let member = extract_member_name(callee, "package dependency")?;
    if member != "package" {
        return Err(ParseError {
            span: *span,
            message: format!("expected `.package(...)`, found `.{member}(...)`"),
        });
    }

    let mut name = None;
    let mut path = None;
    let mut url = None;
    let mut requirement = None;

    for arg in args {
        match arg.label.as_deref() {
            Some("name") => name = Some(expect_string(&arg.value, "package.name")?),
            Some("path") => path = Some(expect_string(&arg.value, "package.path")?),
            Some("url") => url = Some(expect_string(&arg.value, "package.url")?),
            Some("exact") => {
                requirement = Some(VersionRequirement::Exact(expect_string(
                    &arg.value,
                    "package.exact",
                )?))
            }
            Some("from") => {
                requirement = Some(VersionRequirement::From(expect_string(
                    &arg.value,
                    "package.from",
                )?))
            }
            Some("branch") => {
                requirement = Some(VersionRequirement::Branch(expect_string(
                    &arg.value,
                    "package.branch",
                )?))
            }
            Some("revision") => {
                requirement = Some(VersionRequirement::Revision(expect_string(
                    &arg.value,
                    "package.revision",
                )?))
            }
            None => {
                requirement = Some(eval_version_requirement_expr(&arg.value)?);
            }
            Some(other) => {
                return Err(ParseError {
                    span: arg.span,
                    message: format!("unknown argument '{other}' in .package(...)"),
                });
            }
        }
    }

    if let Some(local_path) = path {
        return Ok(PackageDependency::Local {
            name,
            path: local_path,
        });
    }

    if let Some(git_url) = url {
        let req = requirement.ok_or_else(|| ParseError {
            span: *span,
            message: "git `.package(url: ...)` requires version requirement (`exact:`, `from:`, `branch:`, or `.upToNextMajor(...)`)".to_string(),
        })?;
        return Ok(PackageDependency::Git {
            name,
            url: git_url,
            requirement: req,
        });
    }

    Err(ParseError {
        span: *span,
        message: "`.package(...)` requires either `path:` or `url:`".to_string(),
    })
}

fn eval_version_requirement_expr(expr: &SwiftExpr) -> Result<VersionRequirement, ParseError> {
    let SwiftExpr::Call { callee, args, span } = expr else {
        return Err(ParseError {
            span: expr.span(),
            message: "expected version requirement like `.upToNextMajor(from: \"1.0.0\")`"
                .to_string(),
        });
    };
    let name = extract_member_name(callee, "version requirement")?;
    let first = args.first().ok_or_else(|| ParseError {
        span: *span,
        message: format!("missing version string in .{name}(...)"),
    })?;
    let ver = expect_string(&first.value, "version string")?;
    match name.as_str() {
        "exact" => Ok(VersionRequirement::Exact(ver)),
        "upToNextMajor" => Ok(VersionRequirement::UpToNextMajor(ver)),
        "upToNextMinor" => Ok(VersionRequirement::UpToNextMinor(ver)),
        "branch" => Ok(VersionRequirement::Branch(ver)),
        "revision" => Ok(VersionRequirement::Revision(ver)),
        other => Err(ParseError {
            span: *span,
            message: format!("unsupported version requirement '.{other}'"),
        }),
    }
}

fn eval_target(expr: &SwiftExpr) -> Result<TargetSpec, ParseError> {
    let SwiftExpr::Call { callee, args, span } = expr else {
        return Err(ParseError {
            span: expr.span(),
            message: "expected target declaration like `.target(...)` or `.executableTarget(...)`"
                .to_string(),
        });
    };
    let member = extract_member_name(callee, "target constructor")?;
    let kind = match member.as_str() {
        "target" => TargetKind::Regular,
        "executableTarget" => TargetKind::Executable,
        "testTarget" => TargetKind::Test,
        "systemLibrary" => TargetKind::System,
        "binaryTarget" => TargetKind::Binary,
        other => {
            return Err(ParseError {
                span: *span,
                message: format!("unsupported target constructor '.{other}'"),
            });
        }
    };

    let mut name = None;
    let mut path = None;
    let mut sources = Vec::new();
    let mut exclude = Vec::new();
    let mut public_headers_path = None;
    let mut dependencies = Vec::new();
    let mut c_settings = Vec::new();
    let mut cxx_settings = Vec::new();
    let mut swift_settings = Vec::new();
    let mut linker_settings = Vec::new();

    for arg in args {
        let label = arg.label.as_deref().ok_or_else(|| ParseError {
            span: arg.span,
            message: format!("arguments to .{member}(...) must be labeled"),
        })?;
        match label {
            "name" => name = Some(expect_string(&arg.value, "Target.name")?),
            "path" => path = Some(expect_string(&arg.value, "Target.path")?),
            "PublicHeadersPath" | "publicHeadersPath" => {
                public_headers_path = Some(expect_string(&arg.value, "Target.publicHeadersPath")?)
            }
            "sources" => {
                for s in expect_array(&arg.value, "Target.sources")? {
                    sources.push(expect_string(s, "Target.sources element")?);
                }
            }
            "exclude" => {
                for s in expect_array(&arg.value, "Target.exclude")? {
                    exclude.push(expect_string(s, "Target.exclude element")?);
                }
            }
            "dependencies" => {
                for d in expect_array(&arg.value, "Target.dependencies")? {
                    dependencies.push(eval_target_dependency(d)?);
                }
            }
            "cSettings" => {
                for s in expect_array(&arg.value, "Target.cSettings")? {
                    c_settings.push(eval_compiler_setting(s)?);
                }
            }
            "cxxSettings" => {
                for s in expect_array(&arg.value, "Target.cxxSettings")? {
                    cxx_settings.push(eval_compiler_setting(s)?);
                }
            }
            "swiftSettings" => {
                for s in expect_array(&arg.value, "Target.swiftSettings")? {
                    swift_settings.push(eval_compiler_setting(s)?);
                }
            }
            "linkerSettings" => {
                for s in expect_array(&arg.value, "Target.linkerSettings")? {
                    linker_settings.push(eval_linker_setting(s)?);
                }
            }
            other => {
                return Err(ParseError {
                    span: arg.span,
                    message: format!("unknown parameter '{other}' in .{member}(...)"),
                });
            }
        }
    }

    let target_name = name.ok_or_else(|| ParseError {
        span: *span,
        message: format!("missing required `name:` in .{member}(...)"),
    })?;

    Ok(TargetSpec {
        name: target_name,
        kind,
        path,
        sources,
        exclude,
        public_headers_path,
        dependencies,
        c_settings,
        cxx_settings,
        swift_settings,
        linker_settings,
    })
}

fn eval_target_dependency(expr: &SwiftExpr) -> Result<TargetDependency, ParseError> {
    if let SwiftExpr::String(name, _) = expr {
        return Ok(TargetDependency::ByName {
            name: name.clone(),
            condition: None,
        });
    }
    let SwiftExpr::Call { callee, args, span } = expr else {
        return Err(ParseError {
            span: expr.span(),
            message:
                "expected string literal or `.product(...)` / `.target(...)` in target dependencies"
                    .to_string(),
        });
    };
    let member = extract_member_name(callee, "target dependency")?;
    let mut name = None;
    let mut package = None;
    let mut condition = None;

    for (idx, arg) in args.iter().enumerate() {
        match arg.label.as_deref() {
            Some("name") => name = Some(expect_string(&arg.value, "dependency name")?),
            None if idx == 0 && !is_when_call(&arg.value) => {
                name = Some(expect_string(&arg.value, "dependency name")?);
            }
            Some("package") => package = Some(expect_string(&arg.value, "dependency package")?),
            Some("condition") => condition = Some(eval_when_condition(&arg.value)?),
            None if is_when_call(&arg.value) => condition = Some(eval_when_condition(&arg.value)?),
            other => {
                return Err(ParseError {
                    span: arg.span,
                    message: format!("unknown argument '{other:?}' in .{member}(...)"),
                });
            }
        }
    }

    let dep_name = name.ok_or_else(|| ParseError {
        span: *span,
        message: format!("missing `name:` in .{member}(...)"),
    })?;

    match member.as_str() {
        "target" => Ok(TargetDependency::Target {
            name: dep_name,
            condition,
        }),
        "byName" => Ok(TargetDependency::ByName {
            name: dep_name,
            condition,
        }),
        "product" => Ok(TargetDependency::Product {
            name: dep_name,
            package,
            condition,
        }),
        other => Err(ParseError {
            span: *span,
            message: format!("unsupported target dependency '.{other}'"),
        }),
    }
}

fn eval_compiler_setting(expr: &SwiftExpr) -> Result<CompilerSetting, ParseError> {
    let SwiftExpr::Call { callee, args, span } = expr else {
        return Err(ParseError {
            span: expr.span(),
            message: "expected compiler setting like `.define(...)`, `.headerSearchPath(...)`, or `.unsafeFlags(...)`".to_string(),
        });
    };
    let member = extract_member_name(callee, "compiler setting")?;

    match member.as_str() {
        "define" => {
            let mut name = None;
            let mut value = None;
            let mut condition = None;
            for (idx, arg) in args.iter().enumerate() {
                match arg.label.as_deref() {
                    None if idx == 0 => name = Some(expect_string(&arg.value, "define name")?),
                    Some("name") => name = Some(expect_string(&arg.value, "define name")?),
                    Some("to") => value = Some(expect_string(&arg.value, "define value")?),
                    Some("condition") => condition = Some(eval_when_condition(&arg.value)?),
                    None if is_when_call(&arg.value) => {
                        condition = Some(eval_when_condition(&arg.value)?)
                    }
                    other => {
                        return Err(ParseError {
                            span: arg.span,
                            message: format!("unexpected argument '{other:?}' in .define(...)"),
                        });
                    }
                }
            }
            let def_name = name.ok_or_else(|| ParseError {
                span: *span,
                message: "missing macro name in .define(...)".to_string(),
            })?;
            Ok(CompilerSetting::Define {
                name: def_name,
                value,
                condition,
            })
        }
        "headerSearchPath" => {
            let mut path = None;
            let mut condition = None;
            for (idx, arg) in args.iter().enumerate() {
                match arg.label.as_deref() {
                    None if idx == 0 => path = Some(expect_string(&arg.value, "headerSearchPath")?),
                    Some("path") => path = Some(expect_string(&arg.value, "headerSearchPath")?),
                    Some("condition") => condition = Some(eval_when_condition(&arg.value)?),
                    None if is_when_call(&arg.value) => {
                        condition = Some(eval_when_condition(&arg.value)?)
                    }
                    other => {
                        return Err(ParseError {
                            span: arg.span,
                            message: format!(
                                "unexpected argument '{other:?}' in .headerSearchPath(...)"
                            ),
                        });
                    }
                }
            }
            let p = path.ok_or_else(|| ParseError {
                span: *span,
                message: "missing path in .headerSearchPath(...)".to_string(),
            })?;
            Ok(CompilerSetting::HeaderSearchPath { path: p, condition })
        }
        "cStandard" | "cxxStandard" => {
            let mut standard = None;
            let mut condition = None;
            for (idx, arg) in args.iter().enumerate() {
                match arg.label.as_deref() {
                    None if idx == 0 && !is_when_call(&arg.value) => {
                        standard = Some(expect_enum_or_string(&arg.value, &member)?)
                    }
                    Some("condition") => condition = Some(eval_when_condition(&arg.value)?),
                    None if is_when_call(&arg.value) => {
                        condition = Some(eval_when_condition(&arg.value)?)
                    }
                    other => {
                        return Err(ParseError {
                            span: arg.span,
                            message: format!("unexpected argument '{other:?}' in .{member}(...)"),
                        });
                    }
                }
            }
            let std_val = standard.ok_or_else(|| ParseError {
                span: *span,
                message: format!("missing standard value in .{member}(...)"),
            })?;
            Ok(CompilerSetting::Standard {
                standard: std_val,
                condition,
            })
        }
        "unsafeFlags" => {
            let mut flags = Vec::new();
            let mut condition = None;
            for (idx, arg) in args.iter().enumerate() {
                match arg.label.as_deref() {
                    None if idx == 0 && !is_when_call(&arg.value) => {
                        for item in expect_array(&arg.value, "unsafeFlags")? {
                            flags.push(expect_string(item, "unsafeFlags element")?);
                        }
                    }
                    Some("condition") => condition = Some(eval_when_condition(&arg.value)?),
                    None if is_when_call(&arg.value) => {
                        condition = Some(eval_when_condition(&arg.value)?)
                    }
                    other => {
                        return Err(ParseError {
                            span: arg.span,
                            message: format!(
                                "unexpected argument '{other:?}' in .unsafeFlags(...)"
                            ),
                        });
                    }
                }
            }
            Ok(CompilerSetting::UnsafeFlags { flags, condition })
        }
        other => Err(ParseError {
            span: *span,
            message: format!("unsupported compiler setting '.{other}'"),
        }),
    }
}

fn eval_linker_setting(expr: &SwiftExpr) -> Result<LinkerSetting, ParseError> {
    if let SwiftExpr::Member { name, span, .. } = expr {
        if name == "gcSections" {
            return Ok(LinkerSetting::GcSections { condition: None });
        }
        return Err(ParseError {
            span: *span,
            message: format!("unsupported linker setting '.{name}'"),
        });
    }

    let SwiftExpr::Call { callee, args, span } = expr else {
        return Err(ParseError {
            span: expr.span(),
            message: "expected linker setting call".to_string(),
        });
    };
    let member = extract_member_name(callee, "linker setting")?;

    match member.as_str() {
        "linkedLibrary" => {
            let mut library = None;
            let mut condition = None;
            for (idx, arg) in args.iter().enumerate() {
                match arg.label.as_deref() {
                    None if idx == 0 && !is_when_call(&arg.value) => {
                        library = Some(expect_string(&arg.value, "linkedLibrary")?)
                    }
                    Some("condition") => condition = Some(eval_when_condition(&arg.value)?),
                    None if is_when_call(&arg.value) => {
                        condition = Some(eval_when_condition(&arg.value)?)
                    }
                    other => {
                        return Err(ParseError {
                            span: arg.span,
                            message: format!(
                                "unexpected argument '{other:?}' in .linkedLibrary(...)"
                            ),
                        });
                    }
                }
            }
            let lib = library.ok_or_else(|| ParseError {
                span: *span,
                message: "missing library name in .linkedLibrary(...)".to_string(),
            })?;
            Ok(LinkerSetting::LinkedLibrary {
                library: lib,
                condition,
            })
        }
        "linkedFramework" => {
            let mut framework = None;
            let mut condition = None;
            for (idx, arg) in args.iter().enumerate() {
                match arg.label.as_deref() {
                    None if idx == 0 && !is_when_call(&arg.value) => {
                        framework = Some(expect_string(&arg.value, "linkedFramework")?)
                    }
                    Some("condition") => condition = Some(eval_when_condition(&arg.value)?),
                    None if is_when_call(&arg.value) => {
                        condition = Some(eval_when_condition(&arg.value)?)
                    }
                    other => {
                        return Err(ParseError {
                            span: arg.span,
                            message: format!(
                                "unexpected argument '{other:?}' in .linkedFramework(...)"
                            ),
                        });
                    }
                }
            }
            let fw = framework.ok_or_else(|| ParseError {
                span: *span,
                message: "missing framework name in .linkedFramework(...)".to_string(),
            })?;
            Ok(LinkerSetting::LinkedFramework {
                framework: fw,
                condition,
            })
        }
        "gcSections" => {
            let mut condition = None;
            for arg in args {
                if is_when_call(&arg.value) || arg.label.as_deref() == Some("condition") {
                    condition = Some(eval_when_condition(&arg.value)?);
                } else {
                    return Err(ParseError {
                        span: arg.span,
                        message: "unexpected argument in .gcSections(...)".to_string(),
                    });
                }
            }
            Ok(LinkerSetting::GcSections { condition })
        }
        "linkerScript" => {
            let mut path = None;
            let mut condition = None;
            for (idx, arg) in args.iter().enumerate() {
                match arg.label.as_deref() {
                    None if idx == 0 && !is_when_call(&arg.value) => {
                        path = Some(expect_string(&arg.value, "linkerScript")?)
                    }
                    Some("path") => path = Some(expect_string(&arg.value, "linkerScript")?),
                    Some("condition") => condition = Some(eval_when_condition(&arg.value)?),
                    None if is_when_call(&arg.value) => {
                        condition = Some(eval_when_condition(&arg.value)?)
                    }
                    other => {
                        return Err(ParseError {
                            span: arg.span,
                            message: format!(
                                "unexpected argument '{other:?}' in .linkerScript(...)"
                            ),
                        });
                    }
                }
            }
            let p = path.ok_or_else(|| ParseError {
                span: *span,
                message: "missing script path in .linkerScript(...)".to_string(),
            })?;
            Ok(LinkerSetting::LinkerScript { path: p, condition })
        }
        "outputFormat" => {
            let mut format = None;
            let mut family = None;
            let mut condition = None;
            for (idx, arg) in args.iter().enumerate() {
                match arg.label.as_deref() {
                    None if idx == 0 && !is_when_call(&arg.value) => {
                        format = Some(expect_enum_or_string(&arg.value, "outputFormat")?)
                    }
                    Some("format") => {
                        format = Some(expect_enum_or_string(&arg.value, "outputFormat")?)
                    }
                    Some("family") => {
                        family = Some(expect_enum_or_string(&arg.value, "outputFormat.family")?)
                    }
                    Some("condition") => condition = Some(eval_when_condition(&arg.value)?),
                    None if is_when_call(&arg.value) => {
                        condition = Some(eval_when_condition(&arg.value)?)
                    }
                    other => {
                        return Err(ParseError {
                            span: arg.span,
                            message: format!(
                                "unexpected argument '{other:?}' in .outputFormat(...)"
                            ),
                        });
                    }
                }
            }
            let fmt_val = format.ok_or_else(|| ParseError {
                span: *span,
                message: "missing format in .outputFormat(...)".to_string(),
            })?;
            Ok(LinkerSetting::OutputFormat {
                format: fmt_val,
                family,
                condition,
            })
        }
        "unsafeFlags" => {
            let mut flags = Vec::new();
            let mut condition = None;
            for (idx, arg) in args.iter().enumerate() {
                match arg.label.as_deref() {
                    None if idx == 0 && !is_when_call(&arg.value) => {
                        for item in expect_array(&arg.value, "unsafeFlags")? {
                            flags.push(expect_string(item, "unsafeFlags element")?);
                        }
                    }
                    Some("condition") => condition = Some(eval_when_condition(&arg.value)?),
                    None if is_when_call(&arg.value) => {
                        condition = Some(eval_when_condition(&arg.value)?)
                    }
                    other => {
                        return Err(ParseError {
                            span: arg.span,
                            message: format!(
                                "unexpected argument '{other:?}' in .unsafeFlags(...)"
                            ),
                        });
                    }
                }
            }
            Ok(LinkerSetting::UnsafeFlags { flags, condition })
        }
        other => Err(ParseError {
            span: *span,
            message: format!("unsupported linker setting '.{other}'"),
        }),
    }
}

fn is_when_call(expr: &SwiftExpr) -> bool {
    matches!(
        expr,
        SwiftExpr::Call { callee, .. }
            if matches!(callee.as_ref(), SwiftExpr::Member { name, .. } if name == "when")
    )
}

fn eval_when_condition(expr: &SwiftExpr) -> Result<BuildCondition, ParseError> {
    let SwiftExpr::Call { callee, args, span } = expr else {
        return Err(ParseError {
            span: expr.span(),
            message: "expected `.when(...)` condition".to_string(),
        });
    };
    let member = extract_member_name(callee, "condition")?;
    if member != "when" {
        return Err(ParseError {
            span: *span,
            message: format!("expected `.when(...)`, found `.{member}(...)`"),
        });
    }

    let mut platforms = Vec::new();
    let mut configuration = None;
    for arg in args {
        match arg.label.as_deref() {
            Some("platforms") => {
                for p in expect_array(&arg.value, "when.platforms")? {
                    platforms.push(expect_enum_or_string(p, "platform name")?);
                }
            }
            Some("configuration") => {
                configuration = Some(expect_enum_or_string(&arg.value, "when.configuration")?);
            }
            other => {
                return Err(ParseError {
                    span: arg.span,
                    message: format!("unknown argument '{other:?}' in .when(...)"),
                });
            }
        }
    }

    Ok(BuildCondition {
        platforms,
        configuration,
    })
}

fn extract_member_name(expr: &SwiftExpr, context: &str) -> Result<String, ParseError> {
    match expr {
        SwiftExpr::Member { name, .. } | SwiftExpr::Ident(name, _) => Ok(name.clone()),
        _ => Err(ParseError {
            span: expr.span(),
            message: format!("expected member identifier for {context}"),
        }),
    }
}

fn expect_string(expr: &SwiftExpr, context: &str) -> Result<String, ParseError> {
    match expr {
        SwiftExpr::String(s, _) => Ok(s.clone()),
        _ => Err(ParseError {
            span: expr.span(),
            message: format!("expected String literal for {context}"),
        }),
    }
}

fn expect_enum_or_string(expr: &SwiftExpr, context: &str) -> Result<String, ParseError> {
    match expr {
        SwiftExpr::String(s, _) => Ok(s.clone()),
        SwiftExpr::Member { name, .. } | SwiftExpr::Ident(name, _) => Ok(name.clone()),
        _ => Err(ParseError {
            span: expr.span(),
            message: format!("expected enum case (`.value`) or String for {context}"),
        }),
    }
}

fn expect_array<'a>(expr: &'a SwiftExpr, context: &str) -> Result<&'a [SwiftExpr], ParseError> {
    match expr {
        SwiftExpr::Array(items, _) => Ok(items.as_slice()),
        _ => Err(ParseError {
            span: expr.span(),
            message: format!("expected Array `[...]` for {context}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIZUKU_PROTOCOL_PACKAGE: &str = r#"// swift-tools-version: 6.0
import PackageDescription

interface BoardProfile {
    func cpuFlags() -> [String]
    func flashSizeKb(_ multiplier: Int) -> Int
}

struct Pico2WBoard: BoardProfile {
    let baseKb: Int

    func cpuFlags() -> [String] {
        ["-mcpu=cortex-m33", "-mfloat-abi=hard"]
    }

    func flashSizeKb(_ multiplier: Int) -> Int {
        self.baseKb * multiplier
    }
}

let board = Pico2WBoard(baseKb: 2048)

// `Shizuku` conforms to the built-in `Package` protocol/interface!
struct Shizuku: Package {
    let cxxLanguageStandard = .cxx23
    let platforms = [
        .rp2350(board: .pico2W, floatABI: .hard),
        .esp32c5,
        .macOS(.v15),
    ]
    let products = [
        .firmware(name: "shizuku_uf2", format: .uf2, targets: ["shizuku"]),
    ]
    let dependencies = [
        .package(url: "https://github.com/raspberrypi/pico-sdk", exact: "2.2.0"),
        .package(url: "https://gitlab.com/libeigen/eigen", from: "3.4.0"),
    ]
    let targets = [
        .target(
            name: "ShizukuCore",
            path: "firmware/core",
            dependencies: [
                .product(name: "Eigen", package: "eigen"),
                .product(name: "pico_stdlib", package: "pico-sdk", condition: .when(platforms: [.rp2350])),
            ],
            cxxSettings: [
                .unsafeFlags(["-fno-exceptions", "-fno-rtti", "-O0", "-O2"].filter { $0 != "-O0" }),
                .unsafeFlags(board.cpuFlags(), .when(platforms: [.rp2350])),
                .define("SHIZUKU_HOST_SIM", .when(platforms: [.macOS, .linux])),
                .headerSearchPath("include"),
            ] + ["PICO_RP2350", "PICO_CYW43_SUPPORTED"].map {
                .define($0, to: "1", .when(platforms: [.rp2350]))
            }
        ),
        .executableTarget(
            name: "shizuku",
            path: "firmware",
            sources: ["main.cpp"],
            dependencies: [
                "ShizukuCore",
                .product(
                    name: "pico_cyw43_arch_lwip_threadsafe_background",
                    package: "pico-sdk",
                    condition: .when(platforms: [.rp2350])
                ),
            ],
            linkerSettings: [
                .gcSections(.when(platforms: [.rp2350, .esp32c5])),
                .outputFormat(.uf2, family: .rp2350ArmS, .when(platforms: [.rp2350])),
                .linkedLibrary("m"),
            ]
        ),
        .testTarget(
            name: "ShizukuCoreTests",
            dependencies: ["ShizukuCore"]
        ),
    ]
}
"#;

    #[test]
    fn recognizes_struct_conforming_to_package_protocol() {
        let packages =
            parse_packages_swift(SHIZUKU_PROTOCOL_PACKAGE).expect("should parse protocol package");
        assert_eq!(packages.len(), 1);
        let manifest = &packages[0];
        assert_eq!(manifest.name, "Shizuku");
        assert_eq!(
            manifest.origin,
            PackageOrigin::ProtocolConformance {
                type_name: "Shizuku".to_string()
            }
        );
        assert_eq!(manifest.cxx_language_standard.as_deref(), Some("cxx23"));
        assert_eq!(manifest.targets.len(), 3);

        let rp2350_plan = manifest.resolve_plan(Some("rp2350"), Some("release"));
        let core_rp2350 = &rp2350_plan[0];
        assert!(core_rp2350.cxx_flags.contains(&"-std=c++23".to_string()));
        assert!(
            core_rp2350
                .cxx_flags
                .contains(&"-mcpu=cortex-m33".to_string())
        );
        assert!(
            core_rp2350
                .cxx_flags
                .contains(&"-DPICO_RP2350=1".to_string())
        );
        assert!(!core_rp2350.cxx_flags.contains(&"-O0".to_string()));

        let ir = emit_llvm_ir(SHIZUKU_PROTOCOL_PACKAGE, "Shizuku").expect("should emit LLVM IR");
        assert!(ir.contains("@_vtable_Shizuku_as_Package"));
        assert!(ir.contains("@_vtable_Pico2WBoard_as_BoardProfile"));
    }

    #[test]
    fn reports_line_and_column_on_unknown_argument() {
        let bad_src = r#"
import PackageDescription
let package = Package(
    name: "BadPkg",
    unknownField: 123
)
"#;
        let err = parse_package_swift(bad_src).unwrap_err();
        assert_eq!(err.span.line, 5);
        assert!(err.message.contains("unknown parameter 'unknownField'"));
    }
}
