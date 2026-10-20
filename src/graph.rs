//! The dependency graph for JS/TS, PHP, Kotlin/Java and Rust files, built from the working tree.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::{IndexMap, IndexSet};
use serde_json::Value;

use crate::jsonc::{self, JsonValue};
use crate::jsre::{JsRegex, from_units, units};
use crate::kotlin::jvm_graph;
use crate::npath::{self, posix};
use crate::php::{PhpFile, parse_php};
use crate::rust::rust_graph;

const SOURCE_EXT: &[&str] = &[".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"];
const RESOLVE_EXT: &[&str] = &[".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".json"];

fn js_to_ts(ext: &str) -> &'static [&'static str] {
    match ext {
        ".js" => &[".ts", ".tsx"],
        ".jsx" => &[".tsx"],
        ".mjs" => &[".mts"],
        ".cjs" => &[".cts"],
        _ => &[],
    }
}

static PHPUNIT_CONFIG: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^phpunit(\.dist)?\.xml(\.dist)?$", ""));
static BOOTSTRAP: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r#"<phpunit\b[^>]*\bbootstrap\s*=\s*"([^"]+)""#, ""));

#[derive(Default, Debug)]
pub struct Graph {
    /// file -> repo files it imports
    pub imports: IndexMap<String, IndexSet<String>>,
    /// file -> repo files that import it
    pub dependents: IndexMap<String, IndexSet<String>>,
    /// file -> extension-less paths of relative imports that resolved to nothing
    pub unresolved: IndexMap<String, Vec<String>>,
    /// PHP file -> lowercased fully-qualified names it may reference
    pub php_references: IndexMap<String, IndexSet<String>>,
    /// Kotlin/Java file -> fully-qualified names it may reference
    pub jvm_references: IndexMap<String, IndexSet<String>>,
    /// Tests found from the build layout, not the file name: Rust crates (`Cargo.toml`) and integration targets, Kotlin/Java test classes.
    pub test_units: Vec<String>,
    /// Rust package directories ('.' for the root), to find the crate of a removed file.
    pub rust_crates: Vec<String>,
}

pub fn is_source(file: &str) -> bool {
    SOURCE_EXT.contains(&npath::extname(file).as_str()) && !file.ends_with(".d.ts")
}

pub fn is_vendored(f: &str) -> bool {
    f.contains("node_modules/") || f.starts_with("vendor/") || f.contains("/vendor/")
}

/// Builds the dependency graph for JS/TS, PHP, Kotlin/Java and Rust files from the working tree.
/// Err when the tsconfig's `paths` can't be read the way TypeScript's compiler options are shaped.
pub fn build_graph(root: &str, files: &[String]) -> Result<Graph, String> {
    let file_set: IndexSet<String> = files.iter().filter(|f| !is_vendored(f)).cloned().collect();
    let resolver = Resolver::new(root, &file_set)?;
    let mut graph = Graph::default();

    let sources: Vec<&String> = file_set.iter().filter(|f| is_source(f)).collect();
    let specs = par_map(&sources, |file| read(&npath::join(&[root, file])).map(|text| crate::tsimports::imported_files(&text)));
    for (file, specs) in sources.into_iter().zip(specs) {
        let Some(specs) = specs else { continue };
        let mut deps = IndexSet::new();
        for spec in specs {
            if let Some(target) = resolver.resolve(&spec, file) {
                deps.insert(target.clone());
                graph.dependents.entry(target).or_default().insert(file.clone());
            } else if spec.starts_with('.') {
                let stem = strip_ext(&posix::join(&[&posix::dirname(file), &spec]));
                graph.unresolved.entry(file.clone()).or_default().push(stem);
            }
        }
        graph.imports.insert(file.clone(), deps);
    }
    add_php(root, &file_set, &mut graph);
    add_jvm(root, &file_set, &mut graph);
    add_rust(root, &file_set, &mut graph);
    Ok(graph)
}

fn add_edge(graph: &mut Graph, from: &str, to: &str) {
    if from == to {
        return;
    }
    graph.imports.entry(from.to_string()).or_default().insert(to.to_string());
    graph.dependents.entry(to.to_string()).or_default().insert(from.to_string());
}

fn add_php(root: &str, file_set: &IndexSet<String>, graph: &mut Graph) {
    let php: Vec<&String> = file_set.iter().filter(|f| f.ends_with(".php")).collect();
    // unreadable: no edges, so nothing is skipped because of it
    let files = par_map(&php, |file| read(&npath::join(&[root, file])).map(|text| parse_php(file, &text)));
    let parsed: IndexMap<&String, PhpFile> = php.into_iter().zip(files).filter_map(|(file, php)| Some((file, php?))).collect();

    // Index every declared name; duplicates (stubs, fixtures) all count.
    let mut declared_in: IndexMap<&str, Vec<&String>> = IndexMap::new();
    for (file, php) in &parsed {
        for name in php.classes.iter().chain(&php.functions) {
            declared_in.entry(name).or_default().push(file);
        }
    }

    for (file, php) in &parsed {
        graph.imports.entry((*file).clone()).or_default();
        graph.php_references.insert((*file).clone(), php.references.clone());
        for name in &php.references {
            for target in declared_in.get(name.as_str()).map(Vec::as_slice).unwrap_or(&[]) {
                add_edge(graph, file, target);
            }
        }
        for target in &php.includes {
            if file_set.contains(target) {
                add_edge(graph, file, target);
            }
        }
    }

    // Implicit test setup: Pest.php binds base classes by directory, and phpunit.xml names a bootstrap file.
    let tests: Vec<&String> = parsed.keys().filter(|f| f.ends_with("Test.php")).copied().collect();
    for file in file_set {
        let name = posix::basename(file, None);
        let dir = posix::dirname(file);
        let prefix = format!("{dir}/");
        let under = |t: &&&String| dir == "." || t.starts_with(&prefix);
        if name == "Pest.php" {
            for t in tests.iter().filter(under) {
                add_edge(graph, t, file);
            }
        }
        if PHPUNIT_CONFIG.test(&units(&name)) {
            let bootstrap = phpunit_bootstrap(&npath::join(&[root, file]));
            let target = bootstrap.map(|b| posix::normalize(&posix::join(&[&dir, &b])));
            if let Some(target) = target.filter(|t| !t.is_empty() && file_set.contains(t)) {
                for t in tests.iter().filter(under) {
                    add_edge(graph, t, &target);
                }
            }
        }
    }
}

fn add_jvm(root: &str, file_set: &IndexSet<String>, graph: &mut Graph) {
    let jvm = jvm_graph(root, file_set);
    for (file, refs) in jvm.references {
        graph.imports.entry(file.clone()).or_default();
        graph.jvm_references.insert(file, refs);
    }
    for (from, to) in &jvm.edges {
        add_edge(graph, from, to);
    }
    graph.test_units.extend(jvm.units);
}

fn add_rust(root: &str, file_set: &IndexSet<String>, graph: &mut Graph) {
    let rust = rust_graph(root, file_set);
    for unit in &rust.units {
        graph.imports.entry(unit.clone()).or_default();
    }
    for (from, to) in &rust.edges {
        add_edge(graph, from, to);
    }
    // An include of a path that's gone: the file now fails to compile, like a JS import of a removed file.
    for (file, target) in &rust.missing {
        graph.unresolved.entry(file.clone()).or_default().push(strip_ext(target));
    }
    graph.test_units.extend(rust.units);
    graph.rust_crates = rust.crates;
}

fn phpunit_bootstrap(file: &str) -> Option<String> {
    let text = read(file)?;
    let u = units(&text);
    BOOTSTRAP.exec(&u).map(|m| from_units(m.str(1)).into_owned())
}

/// PHP files that reference any of the given names (lowercased, fully qualified).
pub fn php_referencers_of(graph: &Graph, names: &[String]) -> IndexSet<String> {
    graph.php_references.iter().filter(|(_, refs)| names.iter().any(|n| refs.contains(n))).map(|(f, _)| f.clone()).collect()
}

/// Kotlin/Java files that reference any of the given fully-qualified names.
pub fn jvm_referencers_of(graph: &Graph, names: &[String]) -> IndexSet<String> {
    graph.jvm_references.iter().filter(|(_, refs)| names.iter().any(|n| refs.contains(n))).map(|(f, _)| f.clone()).collect()
}

/// All files that transitively import any of `changed` (excluding `changed` itself, unless in a cycle).
pub fn transitive_dependents<'a>(graph: &Graph, changed: impl IntoIterator<Item = &'a String>) -> IndexSet<String> {
    let mut seen: IndexSet<String> = IndexSet::new();
    let mut queue: Vec<String> = changed.into_iter().cloned().collect();
    while let Some(file) = queue.pop() {
        if let Some(deps) = graph.dependents.get(&file) {
            for dep in deps {
                if seen.insert(dep.clone()) {
                    queue.push(dep.clone());
                }
            }
        }
    }
    seen
}

/// Files whose relative imports point at a path that no longer exists (deleted or renamed away).
pub fn importers_of_missing(graph: &Graph, missing: &[String]) -> IndexSet<String> {
    let mut stems: IndexSet<String> = IndexSet::new();
    for m in missing {
        let stem = strip_ext(m);
        stems.insert(stem.strip_suffix("/index").unwrap_or(&stem).to_string());
        stems.insert(stem);
    }
    graph.unresolved.iter().filter(|(_, targets)| targets.iter().any(|t| stems.contains(t))).map(|(f, _)| f.clone()).collect()
}

pub fn strip_ext(p: &str) -> String {
    let ext = npath::extname(p);
    if RESOLVE_EXT.contains(&ext.as_str()) { p[..p.len() - ext.len()].to_string() } else { p.to_string() }
}

struct PathMapping {
    prefix: String,
    suffix: String,
    /// Repo-relative targets with a `*` placeholder.
    targets: Vec<String>,
}

struct WorkspacePackage {
    name: String,
    dir: String,
    entries: Vec<String>,
}

struct Resolver<'a> {
    files: &'a IndexSet<String>,
    paths: Vec<PathMapping>,
    packages: Vec<WorkspacePackage>,
}

impl<'a> Resolver<'a> {
    fn new(root: &str, files: &'a IndexSet<String>) -> Result<Self, String> {
        let paths = read_tsconfig_paths(root)?;
        let packages = read_workspace_packages(root, files);
        Ok(Resolver { files, paths, packages })
    }

    fn try_file(&self, p: &str) -> Option<String> {
        let mut candidates = vec![p.to_string()];
        let ext = npath::extname(p);
        for alt in js_to_ts(&ext) {
            candidates.push(format!("{}{alt}", &p[..p.len() - ext.len()]));
        }
        for e in RESOLVE_EXT {
            candidates.push(format!("{p}{e}"));
        }
        for e in RESOLVE_EXT {
            candidates.push(format!("{p}/index{e}"));
        }
        candidates.into_iter().find(|c| self.files.contains(c))
    }

    fn resolve(&self, spec: &str, from: &str) -> Option<String> {
        if spec.starts_with('.') {
            return self.try_file(&posix::normalize(&posix::join(&[&posix::dirname(from), spec])));
        }

        for PathMapping { prefix, suffix, targets } in &self.paths {
            if !spec.starts_with(prefix.as_str()) || !spec.ends_with(suffix.as_str()) || spec.len() < prefix.len() + suffix.len() {
                continue;
            }
            let star = &spec[prefix.len()..spec.len() - suffix.len()];
            for target in targets {
                if let Some(hit) = self.try_file(&posix::normalize(&crate::js::replace_str(target, "*", star))) {
                    return Some(hit);
                }
            }
        }

        for pkg in &self.packages {
            if spec == pkg.name {
                return pkg.entries.iter().find_map(|e| self.try_file(e));
            }
            if let Some(rest) = spec.strip_prefix(&format!("{}/", pkg.name)) {
                return self.try_file(&posix::join(&[&pkg.dir, rest]));
            }
        }
        None
    }
}

/// The `TypeError` Node's path functions throw for a non-string argument.
fn path_type_error(value: &JsonValue) -> String {
    format!("The \"path\" argument must be of type string. {}", value.received())
}

fn read_tsconfig_paths(root: &str) -> Result<Vec<PathMapping>, String> {
    for name in ["tsconfig.json", "jsconfig.json"] {
        let config = jsonc::try_read_config_file(&npath::join(&[root, name]))?;
        let Some(opts) = config.get("compilerOptions") else { continue };
        let Some(paths) = opts.get("paths").filter(|p| p.truthy()) else { continue };
        let base = match opts.get("baseUrl") {
            None | Some(JsonValue::Undefined | JsonValue::Null) => ".".to_string(),
            Some(JsonValue::String(s)) => posix::normalize(s),
            Some(other) => return Err(path_type_error(other)),
        };
        let mut out = Vec::new();
        for (pattern, targets) in paths.entries() {
            let mut parts = pattern.split('*');
            let prefix = parts.next().unwrap_or("").to_string();
            let suffix = parts.next().unwrap_or("").to_string();
            let targets = match &targets {
                JsonValue::Array(items) => items
                    .iter()
                    .map(|t| match t {
                        JsonValue::String(t) => Ok(posix::normalize(&posix::join(&[&base, t]))),
                        other => Err(path_type_error(other)),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                JsonValue::Undefined => return Err("Cannot read properties of undefined (reading 'map')".into()),
                JsonValue::Null => return Err("Cannot read properties of null (reading 'map')".into()),
                _ => return Err("targets.map is not a function".into()),
            };
            out.push(PathMapping { prefix, suffix, targets });
        }
        return Ok(out);
    }
    Ok(Vec::new())
}

fn nullish(v: Option<&Value>) -> bool {
    matches!(v, None | Some(Value::Null))
}

fn read_workspace_packages(root: &str, files: &IndexSet<String>) -> Vec<WorkspacePackage> {
    let mut out = Vec::new();
    for file in files {
        if !file.ends_with("/package.json") {
            continue;
        }
        // unreadable package.json: imports of it stay unresolved
        let Some(pkg) = crate::json::read(&npath::join(&[root, file])) else { continue };
        if matches!(pkg, Value::Null) {
            continue; // `null.name` throws
        }
        let Some(Value::String(name)) = crate::json::get(&pkg, "name") else { continue };
        let dir = posix::dirname(file);
        let exports = crate::json::get(&pkg, "exports");
        let main = match exports {
            Some(Value::String(_)) => exports,
            _ => {
                let dot = exports.and_then(|e| crate::json::get(e, "."));
                let default = dot.and_then(|d| crate::json::get(d, "default"));
                if !nullish(default) {
                    default
                } else if !nullish(dot) {
                    dot
                } else {
                    crate::json::get(&pkg, "main")
                }
            }
        };
        let mut entries = Vec::new();
        if let Some(Value::String(main)) = main {
            entries.push(posix::join(&[&dir, main]));
        }
        entries.push(format!("{dir}/src/index"));
        entries.push(format!("{dir}/index"));
        out.push(WorkspacePackage { name: name.clone(), dir, entries });
    }
    out
}

fn read(file: &str) -> Option<String> {
    crate::kotlin::read(file)
}

/// `items.iter().map(f)` on every core, in order: reading and parsing files is most of a decision.
pub(crate) fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(items.len());
    if threads <= 1 {
        return items.iter().map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let (next, f) = (&next, &f);
    let work = move || {
        let mut done = Vec::new();
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(i) else { return done };
            done.push((i, f(item)));
        }
    };
    let done: Vec<(usize, R)> = std::thread::scope(|scope| {
        // This thread works too, so a helper that can't start only leaves its share to the others. 8 MiB is
        // the usual main-thread stack: a deeply nested file that parses there parses here.
        let helpers: Vec<_> =
            (1..threads).filter_map(|_| std::thread::Builder::new().stack_size(8 << 20).spawn_scoped(scope, work).ok()).collect();
        let mut done = work();
        for helper in helpers {
            done.extend(helper.join().unwrap_or_else(|e| std::panic::resume_unwind(e)));
        }
        done
    });
    let mut slots: Vec<Option<R>> = std::iter::repeat_with(|| None).take(items.len()).collect();
    for (i, r) in done {
        slots[i] = Some(r);
    }
    slots.into_iter().map(|r| r.expect("every item is mapped")).collect()
}
