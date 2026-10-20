//! Rust dependency extraction, at crate level. A crate compiles as one unit, so its unit
//! tests and doctests depend on every `.rs` file of the package, on the files those sources
//! `include!`, on its build script's inputs, and on the workspace crates it depends on. The
//! crate is a test unit named by its `Cargo.toml`. Each integration test target
//! (`tests/<name>.rs`, `tests/<name>/main.rs`) is a unit of its own, so a change to one test
//! runs only that target.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use indexmap::{IndexMap, IndexSet};

use crate::git::exists;
use crate::glob::Glob;
use crate::js::trim;
use crate::jsre::{JsRegex, from_units, units};
use crate::npath::{self, posix};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dependency {
    pub name: String,
    pub package: Option<String>,
    pub path: Option<String>,
    pub workspace: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Workspace {
    pub members: Vec<String>,
    pub exclude: Vec<String>,
    pub default_members: Option<Vec<String>>,
    pub dependencies: IndexMap<String, Dependency>,
}

/// `package.build`.
#[derive(Clone, Debug, PartialEq)]
pub enum Build {
    /// Not set: the default `build.rs`.
    Default,
    /// `build = false`.
    Off,
    Path(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    /// `[package] name`; None for a virtual workspace manifest.
    pub name: Option<String>,
    pub workspace: Option<Workspace>,
    pub dependencies: Vec<Dependency>,
    /// False when `[[test]]` targets or `autotests = false` make `tests/<name>.rs` names unreliable.
    pub autotests: bool,
    pub build: Build,
}

impl Manifest {
    /// The package name when JavaScript would find it truthy.
    fn package(&self) -> Option<&str> {
        self.name.as_deref().filter(|n| !n.is_empty())
    }
}

const DEPENDENCY_TABLES: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

static WORKSPACE_TABLE: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^\s*\[workspace(?:\.[^\]]*)?\]", "m"));
static TEST_TABLE: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^\s*\[\[\s*test\s*\]\]", "m"));
static INLINE_FIELDS: LazyLock<Vec<(&str, JsRegex)>> = LazyLock::new(|| {
    ["path", "package", "workspace"]
        .into_iter()
        .map(|field| (field, JsRegex::new(&format!(r#"(?:^|[{{,\s]){field}\s*=\s*("(?:\\.|[^"\\])*"|'[^']*'|\w+)"#), "")))
        .collect()
});
static TOML_STRING: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r#""((?:\\.|[^"\\\n])*)"|'([^'\n]*)'"#, "g"));
static TOML_SCALAR: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r#"^\s*(?:"((?:\\.|[^"\\])*)"|'([^']*)')\s*$"#, ""));
static INTEGRATION_TARGET: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^tests\/(?:[^/]+\.rs|[^/]+\/main\.rs)$", ""));
/// Files a build script without `rerun-if-changed` is assumed not to read.
static NOT_BUILD_INPUT: LazyLock<Glob> =
    LazyLock::new(|| Glob::new(&["**/*.{md,mdx,rst,adoc}", "**/LICENSE*", "**/.gitignore"]).expect("valid globs"));
static RERUN: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r#"cargo::?rerun-if-changed=([^"\n]*)"#, "g"));
static LITERAL: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r#""((?:\\.|[^"\\\n])*)""#, "g"));
static NOT_A_PATH: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"[:={}\\]", ""));
// include!/include_str!/include_bytes!("x"), or concat!(env!("CARGO_MANIFEST_DIR"), "/x").
static INCLUDE: LazyLock<JsRegex> = LazyLock::new(|| {
    JsRegex::new(r#"\binclude(?:_str|_bytes)?!\s*\(\s*(concat!\s*\(\s*env!\s*\(\s*"CARGO_MANIFEST_DIR"\s*\)\s*,\s*)?"([^"]+)""#, "g")
});
// Macros that read a path relative to the package: include_dir!("$CARGO_MANIFEST_DIR/assets"), sqlx's migrate!("./migrations").
static PACKAGE_PATH: LazyLock<JsRegex> =
    LazyLock::new(|| JsRegex::new(r#"\b(?:include_dir|migrate)!\s*\(\s*"(?:\$CARGO_MANIFEST_DIR\/)?([^"]+)""#, "g"));
static PATH_ATTRIBUTE: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r#"#\[\s*path\s*=\s*"([^"]+)"\s*\]"#, "g"));
static TEST_TARGET: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^tests\/([^/]+?)(?:\.rs|\/main\.rs)$", ""));
static DOT_PREFIX_OR_SLASH: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^\.\/?|\/$", "g"));

fn real(u: &str) -> String {
    from_units(u).into_owned()
}

pub fn parse_manifest(text: &str) -> Manifest {
    let u = units(text);
    let mut manifest = Manifest {
        name: None,
        workspace: WORKSPACE_TABLE.test(&u).then(Workspace::default),
        dependencies: Vec::new(),
        autotests: !TEST_TABLE.test(&u),
        build: Build::Default,
    };
    let mut listed: IndexMap<String, Dependency> = IndexMap::new();
    let mut workspace_deps: IndexMap<String, Dependency> = IndexMap::new();
    fn dependency<'a>(where_: &'a mut IndexMap<String, Dependency>, kind: &str, name: &str) -> &'a mut Dependency {
        where_.entry(format!("{kind}\0{name}")).or_insert_with(|| Dependency { name: name.to_string(), ..Default::default() })
    }

    for TomlEntry { table, key, value } in toml_entries(text) {
        let t = table.join(".");
        if t == "package" && key.len() == 1 {
            if key[0] == "name" {
                manifest.name = toml_string(&value);
            } else if key[0] == "autotests" && value == "false" {
                manifest.autotests = false;
            } else if key[0] == "build" {
                manifest.build = if value == "false" { Build::Off } else { toml_string(&value).map_or(Build::Default, Build::Path) };
            }
            continue;
        }
        if t == "workspace"
            && key.len() == 1
            && let Some(ws) = manifest.workspace.as_mut()
        {
            if key[0] == "members" {
                ws.members = toml_strings(&value);
            } else if key[0] == "exclude" {
                ws.exclude = toml_strings(&value);
            } else if key[0] == "default-members" {
                ws.default_members = Some(toml_strings(&value));
            }
            continue;
        }
        // [dependencies], [dev-dependencies], [target.'cfg(..)'.dependencies], [workspace.dependencies],
        // and their per-dependency tables ([dependencies.foo]).
        let at = table.iter().enumerate().position(|(i, s)| {
            DEPENDENCY_TABLES.contains(&s.as_str()) && (i == 0 || (i == 1 && table[0] == "workspace") || (i == 2 && table[0] == "target"))
        });
        let Some(at) = at else { continue };
        if table.len() > at + 2 {
            continue;
        }
        let where_ = if table[0] == "workspace" { &mut workspace_deps } else { &mut listed };
        let kind = table[..=at].join(".");
        if table.len() == at + 2 {
            set_field(dependency(where_, &kind, &table[at + 1]), &key.join("."), Some(&value));
        } else if key.len() == 2 {
            set_field(dependency(where_, &kind, &key[0]), &key[1], Some(&value));
        } else if key.len() == 1 {
            let dep = dependency(where_, &kind, &key[0]);
            if value.starts_with('{') {
                for (field, _) in INLINE_FIELDS.iter() {
                    set_field(dep, field, inline_field(&value, field).as_deref());
                }
            }
        }
    }
    manifest.dependencies = listed.into_values().collect();
    if let Some(ws) = manifest.workspace.as_mut() {
        for dep in workspace_deps.into_values() {
            ws.dependencies.insert(dep.name.clone(), dep);
        }
    }
    manifest
}

fn set_field(dep: &mut Dependency, field: &str, value: Option<&str>) {
    let Some(value) = value else { return };
    match field {
        "path" => dep.path = toml_string(value),
        "package" => dep.package = toml_string(value),
        "workspace" => dep.workspace = Some(value == "true"),
        _ => {}
    }
}

fn inline_field(table: &str, field: &str) -> Option<String> {
    let (_, re) = INLINE_FIELDS.iter().find(|(f, _)| *f == field)?;
    let u = units(table);
    re.exec(&u).map(|m| real(m.str(1)))
}

pub struct TomlEntry {
    pub table: Vec<String>,
    pub key: Vec<String>,
    pub value: String,
}

/// Raw `key = value` entries of a TOML document: enough to read Cargo manifests without a TOML parser.
pub fn toml_entries(text: &str) -> Vec<TomlEntry> {
    let mut out = Vec::new();
    let mut table: Vec<String> = Vec::new();
    let lines: Vec<&str> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let mut i = 0;
    while i < lines.len() {
        let lexed = lex_toml(lines[i]).0;
        let line = trim(&lexed);
        if line.is_empty() {
            i += 1;
            continue;
        }
        if line.starts_with('[') {
            let inner = line.strip_prefix("[[").or_else(|| line.strip_prefix('[')).unwrap_or(line);
            let inner = inner.strip_suffix("]]").or_else(|| inner.strip_suffix(']')).unwrap_or(inner);
            table = split_key(inner);
            i += 1;
            continue;
        }
        let Some(eq) = index_outside_quotes(line, '=') else {
            i += 1;
            continue;
        };
        let first = lines[i];
        let mut raw = first.find('=').map_or(first.to_string(), |k| first[k + 1..].to_string());
        // Arrays, inline tables and multi-line strings can span lines.
        while !lex_toml(&raw).1 && i + 1 < lines.len() {
            i += 1;
            raw.push('\n');
            raw.push_str(lines[i]);
        }
        out.push(TomlEntry { table: table.clone(), key: split_key(&line[..eq]), value: trim(&lex_toml(&raw).0).to_string() });
        i += 1;
    }
    out
}

/// Strips comments and reports whether brackets, braces and multi-line strings are closed.
fn lex_toml(s: &str) -> (String, bool) {
    let mut text = String::new();
    let mut depth: i64 = 0;
    let mut quote: Option<&str> = None;
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if let Some(q) = quote {
            if q.len() == 1 && c == '\n' {
                quote = None;
            } else if q.starts_with('"') && c == '\\' {
                let take: usize = rest.chars().take(2).map(char::len_utf8).sum();
                text.push_str(&rest[..take]);
                rest = &rest[take..];
                continue;
            } else if rest.starts_with(q) {
                text.push_str(q);
                rest = &rest[q.len()..];
                quote = None;
                continue;
            }
            text.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        }
        if c == '#' {
            rest = rest.find('\n').map_or("", |nl| &rest[nl..]);
            continue;
        }
        if c == '"' || c == '\'' {
            let q = match c {
                '"' if rest.starts_with("\"\"\"") => "\"\"\"",
                '"' => "\"",
                _ if rest.starts_with("'''") => "'''",
                _ => "'",
            };
            quote = Some(q);
            text.push_str(q);
            rest = &rest[q.len()..];
            continue;
        }
        if c == '[' || c == '{' {
            depth += 1;
        } else if c == ']' || c == '}' {
            depth -= 1;
        }
        text.push(c);
        rest = &rest[c.len_utf8()..];
    }
    let complete = depth <= 0 && quote.is_none_or(|q| q.len() != 3);
    (text, complete)
}

fn split_key(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                current.push(c);
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
        } else if c == '.' {
            out.push(trim(&current).to_string());
            current.clear();
        } else {
            current.push(c);
        }
    }
    out.push(trim(&current).to_string());
    out
}

fn index_outside_quotes(s: &str, char: char) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
        } else if c == char {
            return Some(i);
        }
    }
    None
}

fn toml_string(value: &str) -> Option<String> {
    let u = units(value);
    TOML_SCALAR.exec(&u).map(|m| real(m.get(1).or(m.get(2)).unwrap_or("")))
}

fn toml_strings(value: &str) -> Vec<String> {
    let u = units(value);
    TOML_STRING.match_all(&u).iter().map(|m| real(m.get(1).or(m.get(2)).unwrap_or(""))).collect()
}

/// A crate: a directory with a `Cargo.toml` that has a `[package]`.
struct Crate {
    /// Repo-relative, '.' for the root.
    dir: String,
    manifest: String,
    name: String,
    parsed: Manifest,
}

#[derive(Default, Debug)]
pub struct RustGraph {
    pub edges: Vec<(String, String)>,
    /// Crate manifests and integration test targets.
    pub units: Vec<String>,
    /// A file that names a path not in the repo, with that path.
    pub missing: Vec<(String, String)>,
    /// Crate directories, to find the crate a removed file belonged to.
    pub crates: Vec<String>,
}

pub fn rust_graph(root: &str, files: &IndexSet<String>) -> RustGraph {
    let mut crates: IndexMap<String, Crate> = IndexMap::new();
    let mut workspaces: IndexMap<String, Manifest> = IndexMap::new();
    for file in files {
        if posix::basename(file, None) != "Cargo.toml" {
            continue;
        }
        let Some(text) = read(&npath::join(&[root, file])) else { continue };
        let parsed = parse_manifest(&text);
        let dir = posix::dirname(file);
        if parsed.workspace.is_some() {
            workspaces.insert(dir.clone(), parsed.clone());
        }
        if let Some(name) = parsed.package() {
            let name = name.to_string();
            crates.insert(dir.clone(), Crate { dir, manifest: file.clone(), name, parsed });
        }
    }
    let mut graph = RustGraph { crates: crates.keys().cloned().collect(), ..Default::default() };
    if crates.is_empty() {
        return graph;
    }

    let dirs: IndexSet<String> = crates.keys().cloned().collect();
    let mut owned: HashMap<String, Vec<String>> = HashMap::new();
    for file in files {
        if let Some(dir) = crate_dir_of(&dirs, file) {
            owned.entry(dir).or_default().push(file.clone());
        }
    }
    let under = |dir: &str| -> Vec<String> {
        let prefix = format!("{dir}/");
        files.iter().filter(|f| dir == "." || f.starts_with(&prefix)).cloned().collect()
    };
    let link = |graph: &mut RustGraph, from: &str, target: &str| {
        if files.contains(target) {
            graph.edges.push((from.to_string(), target.to_string()));
        } else {
            let inside = under(target);
            if inside.is_empty() {
                graph.missing.push((from.to_string(), target.to_string()));
            } else {
                for f in inside {
                    graph.edges.push((from.to_string(), f));
                }
            }
        }
    };

    let mut by_name: HashMap<&str, &Crate> = HashMap::new();
    for c in crates.values() {
        by_name.insert(&c.name, c);
    }
    for krate in crates.values() {
        let (manifest, parsed) = (&krate.manifest, &krate.parsed);
        graph.units.push(manifest.clone());
        let crate_files = owned.get(&krate.dir).cloned().unwrap_or_default();
        let rel = |f: &str| -> String { if krate.dir == "." { f.to_string() } else { f[krate.dir.len() + 1..].to_string() } };
        let targets: Vec<String> = if parsed.autotests {
            crate_files.iter().filter(|f| INTEGRATION_TARGET.test(&units(&rel(f)))).cloned().collect()
        } else {
            Vec::new()
        };
        graph.units.extend(targets.iter().cloned());

        let build = build_inputs(root, krate, files);
        for file in &crate_files {
            if file == manifest {
                continue;
            }
            if targets.contains(file) {
                graph.edges.push((file.clone(), manifest.clone()));
            } else if rel(file).starts_with("tests/") {
                // Helpers and fixtures under tests/ belong to every integration test (or to the crate, without per-target units).
                let owners = if targets.is_empty() { std::slice::from_ref(manifest) } else { targets.as_slice() };
                for t in owners {
                    graph.edges.push((t.clone(), file.clone()));
                }
            } else if file.ends_with(".rs") || (build.as_ref().is_some_and(|b| b.all) && !NOT_BUILD_INPUT.is_match(file)) {
                graph.edges.push((manifest.clone(), file.clone()));
            }
            if file.ends_with(".rs")
                && let Some(text) = read(&npath::join(&[root, file]))
            {
                for target in source_includes(&krate.dir, file, &text) {
                    link(&mut graph, file, &target);
                }
            }
        }
        for input in build.map(|b| b.paths).unwrap_or_default() {
            if files.contains(&input) || !under(&input).is_empty() {
                link(&mut graph, manifest, &input);
            }
        }

        for dep in &parsed.dependencies {
            for target in resolve_dependency(krate, dep, &crates, &by_name, &workspaces) {
                if !std::ptr::eq(target, krate) {
                    graph.edges.push((manifest.clone(), target.manifest.clone()));
                }
            }
        }
    }
    graph
}

/// The deepest crate directory in `dirs` that contains `file`, or None.
pub fn crate_dir_of(dirs: &IndexSet<String>, file: &str) -> Option<String> {
    let mut dir = posix::dirname(file);
    loop {
        if dirs.contains(&dir) {
            return Some(dir);
        }
        if dir == "." || dir == "/" {
            return None;
        }
        dir = posix::dirname(&dir);
    }
}

fn resolve_dependency<'a>(
    krate: &'a Crate,
    dep: &Dependency,
    crates: &'a IndexMap<String, Crate>,
    by_name: &HashMap<&str, &'a Crate>,
    workspaces: &IndexMap<String, Manifest>,
) -> Vec<&'a Crate> {
    let (mut dep_path, mut pkg) = (dep.path.clone(), dep.package.clone());
    let mut base = krate.dir.clone();
    if dep.workspace == Some(true) {
        let mut dir = krate.dir.clone();
        loop {
            let shared = workspaces.get(&dir).and_then(|w| w.workspace.as_ref()).and_then(|w| w.dependencies.get(&dep.name));
            if let Some(shared) = shared {
                (dep_path, pkg) = (shared.path.clone(), shared.package.clone());
                base = dir;
                break;
            }
            if dir == "." {
                break;
            }
            dir = posix::dirname(&dir);
        }
    }
    let mut out = Vec::new();
    if let Some(dep_path) = dep_path.filter(|p| !p.is_empty()) {
        let joined = posix::normalize(&posix::join(&[&base, &dep_path]));
        let joined = joined.strip_suffix('/').unwrap_or(&joined);
        let key = if joined.is_empty() { "." } else { joined };
        if let Some(by_path) = crates.get(key) {
            out.push(by_path);
        }
    }
    // By name too: a renamed or workspace-inherited dependency still names the crate. An extra edge only selects more.
    if let Some(named) = by_name.get(pkg.as_deref().unwrap_or(&dep.name)) {
        out.push(named);
    }
    out
}

struct BuildInputs {
    all: bool,
    paths: Vec<String>,
}

/// What a build script reads. Cargo reruns a script without `rerun-if-changed` on any change in
/// its package, so all of the package counts (docs aside). Paths written as string literals in the
/// script (`compile_protos(&["proto/api.proto"])`) count too: such tools print their own
/// `rerun-if-changed` at build time, where picky can't see it.
fn build_inputs(root: &str, krate: &Crate, files: &IndexSet<String>) -> Option<BuildInputs> {
    let script = match &krate.parsed.build {
        Build::Off => return None,
        Build::Default => posix::join(&[&krate.dir, "build.rs"]),
        Build::Path(p) => posix::join(&[&krate.dir, p]),
    };
    if !files.contains(&script) {
        return None;
    }
    let Some(text) = read(&npath::join(&[root, &script])) else {
        return Some(BuildInputs { all: true, paths: Vec::new() });
    };
    let u = units(&text);
    let reruns: Vec<String> = RERUN.match_all(&u).iter().map(|m| real(m.str(1))).collect();
    let literals = LITERAL.match_all(&u);
    let literals = literals.iter().map(|m| m.str(1)).filter(|s| !s.is_empty() && *s != "." && !NOT_A_PATH.test(s)).map(real);
    let paths = reruns
        .iter()
        .filter(|r| !r.contains('{'))
        .cloned()
        .chain(literals)
        .map(|p| {
            let p = posix::normalize(&posix::join(&[&krate.dir, &p]));
            p.strip_suffix('/').map_or(p.clone(), str::to_string)
        })
        .filter(|p| !p.starts_with(".."))
        .collect();
    Some(BuildInputs { all: reruns.is_empty() || reruns.iter().any(|r| r.contains('{')), paths })
}

/// Repo-relative paths a Rust source reads at compile time.
pub fn source_includes(crate_dir: &str, file: &str, text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut add = |base: &str, target: &str| {
        let target = target.strip_prefix('/').unwrap_or(target);
        let resolved = posix::normalize(&posix::join(&[base, target]));
        let resolved = resolved.strip_suffix('/').unwrap_or(&resolved);
        if !resolved.starts_with("..") {
            out.push(resolved.to_string());
        }
    };
    let dir = posix::dirname(file);
    let u = units(text);
    for m in INCLUDE.match_all(&u) {
        add(if m.get(1).is_some_and(|g| !g.is_empty()) { crate_dir } else { &dir }, &real(m.str(2)));
    }
    for m in PACKAGE_PATH.match_all(&u) {
        add(crate_dir, &real(m.str(1)));
    }
    for m in PATH_ATTRIBUTE.match_all(&u) {
        add(&dir, &real(m.str(1)));
    }
    out
}

/// Which crates a cargo command covers, from its working directory.
pub struct CargoScope {
    covers: Box<dyn Fn(&str) -> bool>,
}

impl CargoScope {
    /// True when the crate owning this absolute path is one the command tests.
    pub fn owns(&self, file: &str) -> bool {
        crate_dir_of_abs(file).is_some_and(|c| (self.covers)(&c))
    }
}

/// The packages `cargo test` covers when run in `cwd`: the package there, or, at a workspace root,
/// `default-members` (or every member of a virtual workspace). None when picky can't tell, or when
/// the command reaches crates outside `cwd`, where picky only looks for tests.
pub fn cargo_scope(cwd: &str, workspace_wide: bool, excluded: &[String]) -> Option<CargoScope> {
    let cwd = npath::resolve(&[cwd]);
    let manifest_path = crate::commands::find_up(&cwd, &["Cargo.toml"])?;
    let manifest = read_manifest(&manifest_path)?;
    let dir = npath::dirname(&manifest_path);

    let covers: Box<dyn Fn(&str) -> bool> = if let Some(ws) = manifest.workspace.clone() {
        if dir != cwd {
            return None;
        }
        let has_name = manifest.name.is_some();
        let (dir_m, ws_m) = (dir.clone(), ws.clone());
        let is_member = move |d: &str| {
            (d == dir_m && has_name) || (matches(&ws_m.members, &rel(&dir_m, d)) && !matches(&ws_m.exclude, &rel(&dir_m, d)))
        };
        if workspace_wide {
            let excluded = excluded.to_vec();
            Box::new(move |d| {
                is_member(d) && {
                    let name = read_manifest(&npath::join(&[d, "Cargo.toml"])).and_then(|m| m.name.clone()).unwrap_or_default();
                    !excluded.contains(&name)
                }
            })
        } else if let Some(default_members) = ws.default_members {
            let dir = dir.clone();
            Box::new(move |d| is_member(d) && matches(&default_members, &rel(&dir, d)))
        } else if manifest.package().is_some() {
            let dir = dir.clone();
            Box::new(move |d| d == dir)
        } else {
            Box::new(is_member)
        }
    } else {
        manifest.package()?;
        // `--workspace` in a member directory also tests the members outside it.
        if workspace_wide && workspace_above(&dir) {
            return None;
        }
        let dir = dir.clone();
        Box::new(move |d| d == dir)
    };
    Some(CargoScope { covers })
}

#[derive(Clone, Debug, PartialEq)]
pub struct CargoTarget {
    pub name: String,
    pub test: Option<String>,
}

/// The package and integration test target of a test unit (an absolute path), or None.
pub fn cargo_target(file: &str) -> Option<CargoTarget> {
    let dir = crate_dir_of_abs(file)?;
    let manifest = read_manifest(&npath::join(&[&dir, "Cargo.toml"]))?;
    let name = manifest.package()?.to_string();
    if npath::basename(file) == "Cargo.toml" {
        return Some(CargoTarget { name, test: None });
    }
    let rel = units(&npath::to_posix(&npath::relative(&dir, file))).into_owned();
    let m = TEST_TARGET.exec(&rel)?;
    Some(CargoTarget { name, test: Some(real(m.str(1))) })
}

fn matches(patterns: &[String], rel: &str) -> bool {
    let normalized: Vec<String> =
        patterns.iter().map(|p| real(&DOT_PREFIX_OR_SLASH.replace_all(&units(&posix::normalize(p)), |_| String::new()))).collect();
    if rel.is_empty() {
        return normalized.iter().any(|p| p.is_empty() || p == ".");
    }
    let globs: Vec<&String> = normalized.iter().filter(|p| !p.is_empty()).collect();
    // picomatch throws on a pattern it can't compile; such a member list matches nothing here.
    Glob::plain(&globs).is_ok_and(|g| g.is_match(rel))
}

fn rel(from: &str, to: &str) -> String {
    npath::to_posix(&npath::relative(from, to))
}

fn workspace_above(dir: &str) -> bool {
    let mut d = npath::dirname(dir);
    while d != npath::dirname(&d) {
        if read_manifest(&npath::join(&[&d, "Cargo.toml"])).is_some_and(|m| m.workspace.is_some()) {
            return true;
        }
        if exists(&npath::join(&[&d, ".git"])) {
            return false;
        }
        d = npath::dirname(&d);
    }
    false
}

/// The directory of the package that owns an absolute path.
fn crate_dir_of_abs(file: &str) -> Option<String> {
    let mut dir = npath::dirname(file);
    loop {
        if read_manifest(&npath::join(&[&dir, "Cargo.toml"])).is_some_and(|m| m.package().is_some()) {
            return Some(dir);
        }
        let parent = npath::dirname(&dir);
        if exists(&npath::join(&[&dir, ".git"])) || parent == dir {
            return None;
        }
        dir = parent;
    }
}

static MANIFESTS: LazyLock<Mutex<HashMap<String, Option<Arc<Manifest>>>>> = LazyLock::new(Default::default);

fn read_manifest(file: &str) -> Option<Arc<Manifest>> {
    let mut cache = MANIFESTS.lock().unwrap_or_else(|e| e.into_inner());
    cache.entry(file.to_string()).or_insert_with(|| read(file).map(|text| Arc::new(parse_manifest(&text)))).clone()
}

fn read(file: &str) -> Option<String> {
    crate::kotlin::read(file)
}
