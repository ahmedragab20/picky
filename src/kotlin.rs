//! Kotlin (and Java) dependency extraction. Like PHP, JVM files link by fully-qualified name,
//! not by path, so the graph indexes every top-level declaration, then resolves the names each
//! file mentions: imports (aliases and `*`), same-package names, and fully-qualified names.
//! Names are read from the raw text, strings and comments included (`Class.forName("a.B")`,
//! KDoc links); an extra edge only ever selects more. Java files are parsed too: Gradle compiles
//! and tests them together with Kotlin, and Kotlin code calls Java classes by name.

use std::sync::LazyLock;

use indexmap::{IndexMap, IndexSet};

use crate::git::exists;
use crate::jsre::{JsMatch, JsRegex, from_units, units};
use crate::npath::{self, posix};

#[derive(Clone, Debug, Default)]
pub struct JvmFile {
    pub package: String,
    /// Fully-qualified top-level declarations: classes, objects, interfaces, type aliases, functions, properties.
    pub declarations: Vec<String>,
    /// Fully-qualified names this file may reference.
    pub references: IndexSet<String>,
    /// Top-level classes a test task could run: concrete, with a test annotation in the file or a supertype.
    pub test_classes: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Kotlin,
    Java,
    Groovy,
}

/// `/\.(?:kt|java)$/`
pub fn is_jvm_source(file: &str) -> bool {
    file.ends_with(".kt") || file.ends_with(".java")
}

/// A file under a Gradle/Maven test source root, with the project directory ('' for the root) in group 1.
static TEST_ROOT: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^(?:(.*?)\/)?src\/test\/(?:kotlin|java)\/", ""));
static RESOURCES: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^(?:(.*?)\/)?src\/(?:main|test)\/resources\/", ""));
// TS's TYPE also has a lookbehind before the keyword, `(?<![:.])`, and captures the header in a lookahead,
// `(?=([^{]{0,300}))`. Rust can only run those on the backtracking engine, which tries every start position: about
// 2x slower than TS on a Kotlin repo. `type_declarations` applies both by hand.
static TYPE: LazyLock<JsRegex> = LazyLock::new(|| {
    JsRegex::new(
        r"((?:@[\w.]+(?:\(\s*\))?\s+|\b(?:public|internal|private|protected|abstract|open|sealed|data|final|inner|value|annotation|enum|static|strictfp|expect|actual|external)\s+)*)\b(class|interface|object|typealias|record|enum)\s+([A-Za-z_]\w*)",
        "g",
    )
});
const KEYWORDS: &[&str] = &["class", "interface", "object", "fun", "val", "var"];
static FUN: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"\bfun\s+(?:<[^>\n]*>\s*)?(?:[\w?<>, .]+\.)?([A-Za-z_]\w*)\s*\(", "g"));
static PROPERTY: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"\b(?:val|var)\s+(?:<[^>\n]*>\s*)?(?:[\w?<>]+\.)*([A-Za-z_]\w*)", "g"));
// TS reads an import line with the `m` flag: `^[ \t]*import ...[ \t]*$`. JavaScript's `^` and `$` also match at `\r`,
// U+2028 and U+2029, which Rust can only say with lookarounds (see php.rs). This matches the line terminators
// instead: the one before the line, and the one after it in group 4, which the next search starts from
// (`imports`). Same lines, same groups.
static IMPORT: LazyLock<JsRegex> = LazyLock::new(|| {
    JsRegex::new(
        r"(?:^|[\n\r\u2028\u2029])[ \t]*import\s+(?:static\s+)?([\w.]+?)(\.\*)?(?:\s+as\s+(\w+))?\s*;?[ \t]*(?:$|([\n\r\u2028\u2029]))",
        "g",
    )
});
static NAME: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*", "g"));
static TEST_MARKER: LazyLock<JsRegex> = LazyLock::new(|| {
    JsRegex::new(
        r"@(?:[\w.]+\.)?(?:Test|ParameterizedTest|RepeatedTest|TestFactory|TestTemplate)\b|\b(?:FunSpec|StringSpec|ShouldSpec|DescribeSpec|BehaviorSpec|WordSpec|FreeSpec|FeatureSpec|ExpectSpec|AnnotationSpec|Spek)\s*\(",
        "",
    )
});
static PACKAGE: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^\s*package\s+([\w.]+)", "m"));
static ABSTRACT: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"\b(?:abstract|sealed|annotation|enum)\b|@interface", ""));
static SUPERTYPE: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r":|\bextends\b", ""));
static JVM_NAME: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r#"@file\s*:\s*JvmName\s*\(\s*"(\w+)""#, ""));
static CHAR_LITERAL: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^'(?:\\.[^'\n]{0,5}|[^'\\\n])'", ""));
static COMPUTED_SETTINGS: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"\b(?:projectDir|includeFlat|children)\b", ""));
static INCLUDE: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"(?<![.\w])include\b", "g"));
static OPEN_PAREN: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^\s*\(", ""));
static LITERAL: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r#""([^"$]*)"|'([^']*)'"#, "g"));
static SEPARATORS: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"[\s,]", "g"));
static INCLUDE_BUILD: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r#"\bincludeBuild\s*\(?\s*(?:"([^"$]+)"|'([^']+)')"#, "g"));
static ANDROID: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"android|multiplatform", "i"));

fn real(u: &str) -> String {
    from_units(u).into_owned()
}

/// TS's TYPE matches in `top` as (modifiers, kind, name, header). The lookbehind can only fail when no modifier
/// was read (one ends in whitespace), and every match reading one is tried first, so a failed match is no match at
/// that start: the search resumes one unit later. The lookahead always matches: the header is the next 300 units,
/// up to the first `{`.
fn type_declarations(top: &str) -> Vec<(&str, &str, &str, &str)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(m) = TYPE.exec_at(top, pos) {
        if m.str(1).is_empty() && top[..m.start].ends_with([':', '.']) {
            pos = m.start + 1; // the keyword's first letter
            continue;
        }
        let rest = &top[m.end..];
        let header = rest.char_indices().take_while(|&(_, c)| c != '{').take(300).last().map_or(0, |(i, c)| i + c.len_utf8());
        out.push((m.str(1), m.str(2), m.str(3), &rest[..header]));
        pos = m.end;
    }
    out
}

/// TS's IMPORT matches in `code`: a match ending on a line terminator leaves it to start the next line.
fn imports(code: &str) -> Vec<JsMatch<'_>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(m) = IMPORT.exec_at(code, pos) {
        pos = m.end - m.get(4).map_or(0, str::len);
        out.push(m);
    }
    out
}

pub fn parse_jvm(file: &str, text: &str) -> JvmFile {
    let language = if file.ends_with(".java") { Language::Java } else { Language::Kotlin };
    let text = units(text);
    let code = strip_code(&text, language, false);
    let pkg = PACKAGE.exec(&code).map(|m| real(m.str(1))).unwrap_or_default();
    let qualify = |name: &str| if pkg.is_empty() { name.to_string() } else { format!("{pkg}.{name}") };
    let top = top_level(&code);

    let mut declarations: Vec<String> = Vec::new();
    let mut test_classes: Vec<String> = Vec::new();
    let marked = TEST_MARKER.test(&code);
    for (modifiers, kind, name, header) in type_declarations(&top) {
        let name = real(name);
        if KEYWORDS.contains(&name.as_str()) {
            continue; // `enum class X` seen from `enum`, or `Foo::class` before a declaration
        }
        declarations.push(qualify(&name));
        let concrete = kind == "class" && !ABSTRACT.test(modifiers);
        // A class without a test annotation in its file can still inherit its tests from a supertype.
        if concrete && (marked || SUPERTYPE.test(header)) {
            test_classes.push(qualify(&name));
        }
    }
    if language == Language::Kotlin {
        let members: Vec<String> = FUN.match_all(&top).iter().chain(PROPERTY.match_all(&top).iter()).map(|m| real(m.str(1))).collect();
        declarations.extend(members.iter().map(|m| qualify(m)));
        // Java sees top-level members as static methods of a facade class: FileNameKt, or @file:JvmName.
        if !members.is_empty() {
            let base = posix::basename(file, Some(".kt"));
            let facade = match base.chars().next() {
                Some(c) if c.is_ascii_alphanumeric() || c == '_' => format!("{}{}", c.to_ascii_uppercase(), &base[1..]),
                _ => base.clone(),
            };
            declarations.push(qualify(&format!("{facade}Kt")));
            if let Some(m) = JVM_NAME.exec(&text) {
                let jvm_name = real(m.str(1));
                if !jvm_name.is_empty() {
                    declarations.push(qualify(&jvm_name));
                }
            }
        }
    }

    let mut aliases: IndexMap<String, String> = IndexMap::new();
    let mut wildcards: Vec<String> = Vec::new();
    let mut references: IndexSet<String> = IndexSet::new();
    for m in imports(&code) {
        let name = real(m.str(1));
        if m.get(2).is_some() {
            wildcards.push(name.clone());
        } else {
            let alias = m.get(3).map(real).unwrap_or_else(|| name.rsplit('.').next().unwrap_or("").to_string());
            aliases.insert(alias, name.clone());
        }
        add_prefixes(&mut references, &name);
    }
    let tokens: IndexSet<String> = NAME.match_all(&text).iter().map(|m| real(m.str(0))).collect();
    for token in &tokens {
        add_prefixes(&mut references, token);
        let mut parts = token.split('.');
        let first = parts.next().unwrap_or("");
        let rest: Vec<&str> = parts.collect();
        references.insert(qualify(first));
        if let Some(alias) = aliases.get(first).filter(|a| !a.is_empty()) {
            let mut joined = vec![alias.as_str()];
            joined.extend(&rest);
            add_prefixes(&mut references, &joined.join("."));
        }
        for w in &wildcards {
            references.insert(format!("{w}.{first}"));
        }
    }
    for own in &declarations {
        references.shift_remove(own);
    }
    JvmFile { package: pkg.clone(), declarations, references, test_classes }
}

/// `a.b.C.d` -> a.b, a.b.C, a.b.C.d: a member import or call names its class too.
fn add_prefixes(out: &mut IndexSet<String>, name: &str) {
    let parts: Vec<&str> = name.split('.').collect();
    for i in 1..=parts.len() {
        out.insert(parts[..i].join("."));
    }
}

fn starts_at(text: &[char], i: usize, s: &str) -> bool {
    s.chars().enumerate().all(|(k, c)| text.get(i + k) == Some(&c))
}

/// Blanks out comments, and string contents unless `keep_strings`, keeping offsets and line breaks.
/// Handles Kotlin templates (`"${a["b"]}"`), raw strings and text blocks, nested Kotlin comments
/// and Groovy's single-quoted strings. Works on a unit-space string; returns one.
pub fn strip_code(text: &str, language: Language, keep_strings: bool) -> String {
    let src: Vec<char> = text.chars().collect();
    let mut out = src.clone();
    let len = src.len();
    let blank = |out: &mut Vec<char>, from: usize, to: usize| {
        for c in out.iter_mut().take(to.min(len)).skip(from) {
            if *c != '\n' {
                *c = ' ';
            }
        }
    };
    let blank_string = |out: &mut Vec<char>, from: usize, to: usize| {
        if !keep_strings {
            blank(out, from, to);
        }
    };
    let templates = language != Language::Java;
    struct Frame {
        quote: &'static str,
        braces: i64,
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut i = 0;
    while i < len {
        let c = src[i];
        if let Some(top) = stack.last()
            && top.quote != "${"
        {
            let quote = top.quote;
            if starts_at(&src, i, quote) {
                stack.pop();
                i += quote.len();
            } else if quote.len() == 1 && c == '\n' {
                stack.pop(); // unterminated: stop at the line end
                i += 1;
            } else if c == '\\' && quote != "\"\"\"" {
                blank_string(&mut out, i, i + 2);
                i += 2;
            } else if templates && quote.starts_with('"') && c == '$' && src.get(i + 1) == Some(&'{') {
                blank_string(&mut out, i, i + 2);
                stack.push(Frame { quote: "${", braces: 0 });
                i += 2;
            } else {
                blank_string(&mut out, i, i + 1);
                i += 1;
            }
            continue;
        }
        if starts_at(&src, i, "//") {
            let end = (i..len).find(|&k| src[k] == '\n').unwrap_or(len);
            blank(&mut out, i, end);
            i = end;
            continue;
        }
        if starts_at(&src, i, "/*") {
            let mut depth = 1;
            let mut j = i + 2;
            while j < len && depth > 0 {
                if language == Language::Kotlin && starts_at(&src, j, "/*") {
                    depth += 1;
                    j += 2;
                } else if starts_at(&src, j, "*/") {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            blank(&mut out, i, j);
            i = j;
            continue;
        }
        let quote = ["\"\"\"", "'''", "\"", "'"]
            .into_iter()
            .find(|q| starts_at(&src, i, q) && (q.starts_with('"') || language == Language::Groovy));
        if let Some(quote) = quote {
            stack.push(Frame { quote, braces: 0 });
            i += quote.len();
            continue;
        }
        if c == '\'' {
            let window: String = src[i..(i + 9).min(len)].iter().collect();
            if let Some(m) = CHAR_LITERAL.exec(&window) {
                let n = m.str(0).chars().count();
                blank_string(&mut out, i + 1, i + n - 1);
                i += n;
                continue;
            }
        }
        if let Some(top) = stack.last_mut() {
            // Code inside a template is part of its string.
            if c == '{' {
                top.braces += 1;
            } else if c == '}' {
                let was = top.braces;
                top.braces -= 1;
                if was == 0 {
                    stack.pop();
                }
            }
            blank_string(&mut out, i, i + 1);
        }
        i += 1;
    }
    out.into_iter().collect()
}

/// Code at the top level of the file: everything inside braces and parentheses blanked.
fn top_level(code: &str) -> String {
    let mut out: Vec<char> = code.chars().collect();
    let mut depth: i64 = 0;
    for c in out.iter_mut() {
        if *c == '{' || *c == '(' {
            if depth > 0 {
                *c = ' ';
            }
            depth += 1;
        } else if *c == '}' || *c == ')' {
            depth = (depth - 1).max(0);
            if depth > 0 {
                *c = ' ';
            }
        } else if depth > 0 && *c != '\n' {
            *c = ' ';
        }
    }
    out.into_iter().collect()
}

#[derive(Default)]
pub struct JvmGraph {
    pub edges: Vec<(String, String)>,
    /// Test sources that declare a class a test task can run.
    pub units: Vec<String>,
    pub references: IndexMap<String, IndexSet<String>>,
}

/// Group 1 of TEST_ROOT/RESOURCES on a path: Some("") when the root project matched.
fn project_dir(re: &JsRegex, file: &str) -> Option<String> {
    let u = units(file);
    re.exec(&u).map(|m| m.get(1).map(real).unwrap_or_default())
}

pub fn jvm_graph(root: &str, files: &IndexSet<String>) -> JvmGraph {
    let sources: Vec<&String> = files.iter().filter(|f| is_jvm_source(f)).collect();
    // unreadable: no edges, so nothing is skipped because of it
    let jvms = crate::graph::par_map(&sources, |file| read(&npath::join(&[root, file])).map(|text| parse_jvm(file, &text)));
    let parsed: IndexMap<&String, JvmFile> = sources.into_iter().zip(jvms).filter_map(|(file, jvm)| Some((file, jvm?))).collect();
    let mut graph = JvmGraph::default();
    let mut declared_in: IndexMap<&str, Vec<&String>> = IndexMap::new();
    for (file, jvm) in &parsed {
        for name in &jvm.declarations {
            declared_in.entry(name.as_str()).or_default().push(file);
        }
    }
    for (file, jvm) in &parsed {
        graph.references.insert((*file).clone(), jvm.references.clone());
        for name in &jvm.references {
            for target in declared_in.get(name.as_str()).map(Vec::as_slice).unwrap_or(&[]) {
                if target != file {
                    graph.edges.push(((*file).clone(), (*target).clone()));
                }
            }
        }
        if project_dir(&TEST_ROOT, file).is_some() && !jvm.test_classes.is_empty() {
            graph.units.push((*file).clone());
        }
    }

    // Tests read their project's resources from the classpath (application.yml, fixtures).
    let mut resources: IndexMap<String, Vec<String>> = IndexMap::new();
    for file in files {
        if let Some(dir) = project_dir(&RESOURCES, file) {
            resources.entry(dir).or_default().push(file.clone());
        }
    }
    for unit in &graph.units.clone() {
        let dir = project_dir(&TEST_ROOT, unit).unwrap_or_default();
        for r in resources.get(&dir).map(Vec::as_slice).unwrap_or(&[]) {
            graph.edges.push((unit.clone(), r.clone()));
        }
    }
    graph
}

/// Declared names of a file's old content, to find who used a deleted file.
pub fn jvm_declared_names(file: &str, text: &str) -> Vec<String> {
    parse_jvm(file, text).declarations
}

/// The runnable test classes of a test source (an absolute path), or none when it can't be read.
pub fn jvm_test_classes(file: &str) -> Vec<String> {
    read(file).map(|text| parse_jvm(file, &text).test_classes).unwrap_or_default()
}

const SETTINGS: &[&str] = &["settings.gradle.kts", "settings.gradle"];
const BUILD_FILES: &[&str] = &["build.gradle.kts", "build.gradle"];

#[derive(Clone, Debug, PartialEq)]
pub struct GradleProject {
    /// Directory relative to the build root, '' for the root project.
    pub dir: String,
    /// Gradle path: ':' for the root project, ':a:b' for a/b.
    pub path: String,
}

pub struct GradleBuild {
    pub root: String,
    projects: IndexMap<String, String>,
}

impl GradleBuild {
    /// The project whose `src/test` holds this absolute path, or None when no project of this build does.
    pub fn project_of(&self, file: &str) -> Option<GradleProject> {
        let rel = npath::to_posix(&npath::relative(&self.root, file));
        if rel.starts_with("..") {
            return None;
        }
        let dir = project_dir(&TEST_ROOT, &rel)?;
        let path = self.projects.get(&dir).filter(|p| !p.is_empty())?;
        Some(GradleProject { dir, path: path.clone() })
    }

    /// True when the project's `test` task can't take `--tests` (Android, Kotlin Multiplatform).
    pub fn cant_filter(&self, project: &GradleProject) -> bool {
        let dir = npath::join(&[&self.root, &project.dir]);
        let build: Vec<String> = BUILD_FILES.iter().map(|f| read(&npath::join(&[&dir, f])).unwrap_or_default()).collect();
        ANDROID.test(&units(&build.join("\n")))
            || ["src/main/AndroidManifest.xml", "src/commonMain", "src/commonTest"].iter().any(|f| exists(&npath::join(&[&dir, f])))
    }
}

/// The Gradle build `cwd` belongs to, with its projects read from the settings file. None when the
/// settings compute their projects (non-literal `include`, custom `projectDir`): picky can't map
/// test files to task paths there.
pub fn gradle_build(cwd: &str) -> Option<GradleBuild> {
    let cwd = npath::resolve(&[cwd]);
    let settings = crate::commands::find_up(&cwd, SETTINGS);
    let mut projects: IndexMap<String, String> = IndexMap::from([(String::new(), ":".to_string())]);
    let mut root = cwd.clone();
    if let Some(settings) = settings {
        root = npath::dirname(&settings);
        let language = if settings.ends_with(".kts") { Language::Kotlin } else { Language::Groovy };
        let included = settings_includes(&read(&settings).unwrap_or_default(), language)?;
        for name in included {
            let segments: Vec<&str> = name.split(':').filter(|s| !s.is_empty()).collect();
            for k in 1..=segments.len() {
                projects.insert(segments[..k].join("/"), format!(":{}", segments[..k].join(":")));
            }
        }
    } else if !BUILD_FILES.iter().any(|f| exists(&npath::join(&[&cwd, f]))) {
        return None;
    }
    Some(GradleBuild { root, projects })
}

/// Project paths named by `include` in a settings file, or None when any of them isn't a literal.
pub fn settings_includes(text: &str, language: Language) -> Option<Vec<String>> {
    let text = units(text);
    let code = strip_code(&text, language, true);
    if COMPUTED_SETTINGS.test(&code) {
        return None;
    }
    let mut out = Vec::new();
    for m in INCLUDE.match_all(&code) {
        let rest = &code[m.end..];
        let args: String = if OPEN_PAREN.test(rest) {
            let close = rest.find(')')?;
            let open = rest.find('(').expect("matched");
            if open + 1 > close { String::new() } else { rest[open + 1..close].to_string() }
        } else {
            // Groovy: `include 'a', 'b'`, possibly continued after a trailing comma.
            let lines: Vec<&str> = rest.split('\n').collect();
            let mut args = lines[0].to_string();
            let mut k = 1;
            while crate::js::trim_end(&args).ends_with(',') && k < lines.len() {
                args.push('\n');
                args.push_str(lines[k]);
                k += 1;
            }
            args
        };
        let literals = LITERAL.match_all(&args);
        let leftover = LITERAL.replace_all(&args, |_| String::new());
        if !SEPARATORS.replace_all(&leftover, |_| String::new()).is_empty() {
            return None;
        }
        out.extend(literals.iter().map(|l| real(l.get(1).or(l.get(2)).unwrap_or(""))));
    }
    Some(out)
}

/// Directories of builds included with `includeBuild` (build logic, convention plugins), repo-relative.
pub fn included_builds(root: &str, files: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for file in files {
        if !SETTINGS.contains(&posix::basename(file, None).as_str()) {
            continue;
        }
        let language = if file.ends_with(".kts") { Language::Kotlin } else { Language::Groovy };
        let text = units(&read(&npath::join(&[root, file])).unwrap_or_default()).into_owned();
        let code = strip_code(&text, language, true);
        for m in INCLUDE_BUILD.match_all(&code) {
            let target = real(m.get(1).or(m.get(2)).unwrap_or(""));
            let dir = posix::normalize(&posix::join(&[&posix::dirname(file), &target]));
            let dir = dir.strip_suffix('/').map_or(dir.clone(), str::to_string);
            if !dir.starts_with("..") && dir != "." {
                out.push(dir);
            }
        }
    }
    out
}

const INIT_SCRIPT: &str = "// Written by picky. A narrowed run filters each test task to the selected classes;
// a class without tests (a helper or base class) must not fail the task.
allprojects {
    tasks.withType(Test).configureEach {
        filter.failOnNoMatchingTests = false
    }
}
";

/// An init script that lets a test filter match nothing. None when it can't be written.
pub fn gradle_init_script() -> Option<String> {
    let file = npath::join(&[&npath::tmpdir(), "picky-1-no-matching-tests.init.gradle"]);
    if read(&file).as_deref() != Some(INIT_SCRIPT) {
        std::fs::write(&file, INIT_SCRIPT).ok()?;
    }
    Some(file)
}

pub(crate) fn read(file: &str) -> Option<String> {
    std::fs::read(file).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
}
