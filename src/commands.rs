use std::collections::BTreeSet;
use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};
use serde_json::Value;

use crate::git::exists;
use crate::kotlin::{gradle_build, gradle_init_script, jvm_test_classes};
use crate::rust::{cargo_scope, cargo_target};
use crate::sh::{split_assignments, tokenize};
use crate::{json, npath};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ecosystem {
    Js,
    Php,
    Kotlin,
    Rust,
}

impl Ecosystem {
    pub const ALL: [Ecosystem; 4] = [Ecosystem::Js, Ecosystem::Php, Ecosystem::Kotlin, Ecosystem::Rust];

    pub fn as_str(self) -> &'static str {
        match self {
            Ecosystem::Js => "js",
            Ecosystem::Php => "php",
            Ecosystem::Kotlin => "kotlin",
            Ecosystem::Rust => "rust",
        }
    }
}

type NarrowTests = Rc<dyn Fn(&[String]) -> Option<Vec<String>>>;
type NarrowFiles = Rc<dyn Fn(&[String]) -> Vec<String>>;

/// Runs test files: picky narrows it to the selected ones.
#[derive(Clone)]
pub struct TestCommand {
    pub tool: String,
    pub ecosystem: Ecosystem,
    /// The argv that runs only `files` (relative to the cwd), or None when the runner can't take them all.
    pub narrow: NarrowTests,
    /// Why `narrow` can return None, for the log.
    pub why_not: Option<String>,
    /// Whether a test file (relative to the cwd) is one this command runs, when not every test under the cwd is.
    pub owns: Option<Owns>,
}

pub type Owns = Rc<dyn Fn(&str) -> bool>;

/// Checks files one by one (a linter or formatter): picky narrows it to the affected files.
#[derive(Clone)]
pub struct FilesCommand {
    pub tool: String,
    pub ecosystem: Ecosystem,
    /// What the command checks, relative to its cwd.
    pub targets: Vec<String>,
    /// True when a file's result can depend on files it imports (type-aware lint rules).
    pub with_dependents: bool,
    pub narrow: NarrowFiles,
}

/// Checks the project as a whole (a type checker or static analyzer): picky runs it in full or skips it.
#[derive(Clone, Debug)]
pub struct CheckCommand {
    pub tool: String,
    pub ecosystem: Ecosystem,
}

#[derive(Clone)]
pub enum Command {
    Tests(TestCommand),
    Files(FilesCommand),
    Check(CheckCommand),
}

impl Command {
    pub fn ecosystem(&self) -> Ecosystem {
        match self {
            Command::Tests(c) => c.ecosystem,
            Command::Files(c) => c.ecosystem,
            Command::Check(c) => c.ecosystem,
        }
    }
}

fn check(tool: &str, ecosystem: Ecosystem) -> Option<Command> {
    Some(Command::Check(CheckCommand { tool: tool.into(), ecosystem }))
}

pub fn ecosystem_of(file: &str) -> Ecosystem {
    if file.ends_with(".php") {
        return Ecosystem::Php;
    }
    if file.ends_with(".rs") || npath::posix::basename(file, None) == "Cargo.toml" {
        return Ecosystem::Rust;
    }
    if file.ends_with(".kt") || file.ends_with(".kts") || file.ends_with(".java") {
        return Ecosystem::Kotlin;
    }
    Ecosystem::Js
}

const MAX_SCRIPT_DEPTH: usize = 3;

fn check_ecosystem(tool: &str) -> Option<Ecosystem> {
    Some(match tool {
        "tsc" | "vue-tsc" | "svelte-check" | "biome" => Ecosystem::Js,
        "phpstan" | "psalm" | "phpcs" | "php-cs-fixer" | "pint" | "rector" => Ecosystem::Php,
        "detekt" | "detekt-cli" => Ecosystem::Kotlin,
        _ => return None,
    })
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn concat(parts: &[&[String]]) -> Vec<String> {
    parts.iter().flat_map(|p| p.iter().cloned()).collect()
}

/// Recognizes a test, lint or type-check command. Returns None when the command is
/// unknown or already targets specific tests; picky then runs it unchanged. Err is an error
/// reading the project (a malformed composer.lock).
pub fn recognize(argv: &[String], cwd: &str) -> Result<Option<Command>, String> {
    recognize_at(argv, cwd, 0)
}

fn recognize_at(argv: &[String], cwd: &str, depth: usize) -> Result<Option<Command>, String> {
    let rest = split_assignments(argv).rest;
    let prefix_len = argv.len() - rest.len();
    if rest.is_empty() {
        return Ok(None);
    }

    let mut i = prefix_len;
    // Launchers that run a local binary: npx [-y] X, bunx X, pnpm|yarn|bun exec X, php [-d k=v] X.
    if argv[i] == "npx" || argv[i] == "bunx" {
        i += 1;
        while i < argv.len() && argv[i].starts_with('-') {
            i += 1;
        }
    } else if ["pnpm", "yarn", "bun"].contains(&argv[i].as_str()) && argv.get(i + 1).is_some_and(|a| a == "exec") {
        i += 2;
    } else if argv[i] == "php" {
        i += 1;
        while i < argv.len() && argv[i].starts_with('-') {
            if argv[i] == "-d" {
                i += 2;
            } else if argv[i]
                .strip_prefix("-d")
                .and_then(|r| r.chars().next())
                .is_some_and(|c| !matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
            {
                // /^-d./
                i += 1;
            } else {
                return Ok(None);
            }
        }
    }
    if i >= argv.len() {
        return Ok(None);
    }

    let before: Vec<String> = argv[..=i].to_vec();
    let base = npath::basename(&argv[i]);
    let tool = ["phar", "bat", "cmd"].iter().find_map(|ext| base.strip_suffix(&format!(".{ext}"))).unwrap_or(&base).to_string();
    let args: Vec<String> = argv[i + 1..].to_vec();

    if let Some(eco) = check_ecosystem(&tool) {
        return Ok(check(&tool, eco));
    }

    Ok(match tool.as_str() {
        "vitest" => {
            let sub: Vec<String> = if args.first().is_some_and(|a| a == "run") { strs(&["run"]) } else { vec![] };
            let flags: Vec<String> = args[sub.len()..].to_vec();
            if !only_flags(&flags) {
                return Ok(None);
            }
            tests("vitest", Ecosystem::Js, Rc::new(move |files| Some(concat(&[&before, &sub, &flags, files]))))
        }
        "jest" => {
            if !only_flags(&args) {
                return Ok(None);
            }
            let by_path = strs(&["--runTestsByPath"]);
            tests("jest", Ecosystem::Js, Rc::new(move |files| Some(concat(&[&before, &args, &by_path, files]))))
        }
        "node" => {
            if !args.iter().any(|a| a == "--test") || !only_flags(&args) {
                return Ok(None);
            }
            tests("node-test", Ecosystem::Js, Rc::new(move |files| Some(concat(&[&before, &args, files]))))
        }
        "phpunit" | "pest" => {
            // Paths replace the configured suites; an explicit suite would conflict with them.
            if !only_flags(&args) || args.iter().any(|a| a.starts_with("--testsuite")) {
                return Ok(None);
            }
            // PHPUnit 9 silently runs only the first of several paths.
            let several_ok = phpunit_major(cwd)? >= 10.0;
            Some(Command::Tests(TestCommand {
                tool: tool.clone(),
                ecosystem: Ecosystem::Php,
                narrow: Rc::new(move |files| if files.len() > 1 && !several_ok { None } else { Some(concat(&[&before, &args, files])) }),
                why_not: Some(format!("{tool} before 10 runs only the first of several paths")),
                owns: None,
            }))
        }
        "eslint" => eslint(before, &args, cwd),
        "prettier" => prettier(before, &args),
        "npm" | "pnpm" | "yarn" => return recognize_script(&tool, &argv[..i], &args, cwd, depth),
        "composer" => return recognize_composer(&argv[..=i], &args, cwd, depth),
        "cargo" => cargo(&before, &args, cwd),
        "gradle" | "gradlew" => gradle(&before, &args, cwd),
        "ktlint" => ktlint(before, &args),
        _ => None,
    })
}

fn tests(tool: &str, ecosystem: Ecosystem, narrow: NarrowTests) -> Option<Command> {
    Some(Command::Tests(TestCommand { tool: tool.into(), ecosystem, narrow, why_not: None, owns: None }))
}

fn recognize_script(pm: &str, prefix: &[String], args: &[String], cwd: &str, depth: usize) -> Result<Option<Command>, String> {
    if depth >= MAX_SCRIPT_DEPTH || args.iter().any(|a| a == "--") {
        return Ok(None);
    }
    // Package-manager flags (--silent, --if-present, ...) are kept; the words must name a script.
    let words: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let pm_flags: Vec<String> = args.iter().filter(|a| a.starts_with('-')).cloned().collect();
    let script = if words.len() == 1 && (words[0] == "test" || words[0] == "t") {
        "test".to_string()
    } else if words.len() == 2 && (words[0] == "run" || words[0] == "run-script") {
        words[1].clone()
    } else {
        return Ok(None);
    };
    if script.is_empty() {
        return Ok(None);
    }

    let package = json::read(&npath::join(&[cwd, "package.json"]));
    let scripts = package
        .as_ref()
        .and_then(|p| json::get(p, "scripts"))
        .filter(|s| !s.is_null())
        .cloned()
        .unwrap_or(Value::Object(Default::default()));
    let Some(body_words) = json::get_str(&scripts, &script).and_then(|body| tokenize(&body)) else { return Ok(None) };
    let Some(inner) = recognize_at(&body_words, cwd, depth + 1)? else { return Ok(None) };

    Ok(match inner {
        Command::Check(_) => Some(inner),
        Command::Files(inner) => {
            // Forwarded files would be added to the script's own targets (`eslint . a.ts` checks everything),
            // so the narrowed body runs directly with the package's local binary.
            if json::truthy(json::get(&scripts, &format!("pre{script}"))) || json::truthy(json::get(&scripts, &format!("post{script}"))) {
                return Ok(None);
            }
            let Some(bin) = local_bin(cwd, &inner.tool) else { return Ok(None) };
            let env_len = split_assignments(&body_words).env.len();
            let prefix = prefix.to_vec();
            let tool = inner.tool.clone();
            let narrow = Rc::clone(&inner.narrow);
            Some(Command::Files(FilesCommand {
                narrow: Rc::new(move |files| {
                    let narrowed = narrow(files);
                    let at = narrowed.iter().position(|w| *w == tool);
                    if at != Some(env_len) {
                        return concat(&[&prefix, &narrowed]); // launcher in the body: keep it
                    }
                    concat(&[&prefix, &narrowed[..env_len], std::slice::from_ref(&bin), &narrowed[env_len + 1..]])
                }),
                ..inner
            }))
        }
        Command::Tests(inner) => {
            // npm needs `--` to forward arguments to the script; pnpm and yarn forward them directly.
            let call = concat(&[prefix, &strs(&[pm]), &pm_flags, &strs(&["run"]), std::slice::from_ref(&script)]);
            let npm = pm == "npm";
            let narrow = Rc::clone(&inner.narrow);
            Some(Command::Tests(TestCommand {
                narrow: Rc::new(move |files| {
                    narrow(files)?;
                    Some(if npm { concat(&[&call, &strs(&["--"]), files]) } else { concat(&[&call, files]) })
                }),
                ..inner
            }))
        }
    })
}

fn recognize_composer(call: &[String], args: &[String], cwd: &str, depth: usize) -> Result<Option<Command>, String> {
    if depth >= MAX_SCRIPT_DEPTH || args.iter().any(|a| a == "--") {
        return Ok(None);
    }
    let words: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let flags: Vec<String> = args.iter().filter(|a| a.starts_with('-')).cloned().collect();
    let script = if words.len() == 2 && (words[0] == "run" || words[0] == "run-script") {
        words[1].clone()
    } else if words.len() == 1 {
        words[0].clone()
    } else {
        return Ok(None);
    };
    if script.is_empty() {
        return Ok(None);
    }

    let composer = json::read(&npath::join(&[cwd, "composer.json"]));
    let entry = composer.as_ref().and_then(|c| json::get(c, "scripts")).filter(|s| !s.is_null());
    let body = match entry {
        Some(scripts) => match scripts {
            Value::String(_) => json::get_str(scripts, &script),
            _ => script_body(json::get(scripts, &script)),
        },
        None => None,
    };
    let Some(body) = body.filter(|b| !b.is_empty()) else { return Ok(None) };
    let body_words = if let Some(rest) = body.strip_prefix("@php ") {
        tokenize(&format!("php {rest}"))
    } else if let Some(rest) = body.strip_prefix("@composer ") {
        tokenize(&format!("composer {rest}"))
    } else if is_script_reference(&body) {
        Some(vec!["composer".to_string(), "run-script".to_string(), body[1..].to_string()])
    } else if body.starts_with('@') {
        return Ok(None);
    } else {
        tokenize(&body)
    };
    let Some(body_words) = body_words else { return Ok(None) };

    let inner = match recognize_at(&body_words, cwd, depth + 1)? {
        None | Some(Command::Files(_)) => return Ok(None),
        Some(c @ Command::Check(_)) => return Ok(Some(c)),
        Some(Command::Tests(t)) => t,
    };
    // Without `--`, composer drops every argument when an option comes first.
    let run = concat(&[call, &flags, &strs(&["run-script"]), std::slice::from_ref(&script), &strs(&["--"])]);
    let narrow = Rc::clone(&inner.narrow);
    Ok(Some(Command::Tests(TestCommand {
        narrow: Rc::new(move |files| {
            narrow(files)?;
            Some(concat(&[&run, files]))
        }),
        ..inner
    })))
}

/// `/^@[\w:-]+$/`
fn is_script_reference(body: &str) -> bool {
    body.len() > 1 && body.starts_with('@') && body[1..].bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b':' || c == b'-')
}

/// A composer script body: a string, or an array holding exactly one command.
fn script_body(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) if items.len() == 1 => items[0].as_str().map(str::to_string),
        _ => None,
    }
}

// Cargo options that take a value as the next word. Package and target selection (-p, --lib,
// --test, a test name) means the command already targets specific tests, so it isn't listed.
const CARGO_VALUE: &[&str] = &[
    "--features",
    "-F",
    "--target",
    "--target-dir",
    "--profile",
    "-j",
    "--jobs",
    "--color",
    "--message-format",
    "--config",
    "-Z",
    "--exclude",
    "--lockfile-path",
];
const CARGO_BOOLEAN: &[&str] = &[
    "--workspace",
    "--all",
    "--all-features",
    "--no-default-features",
    "--release",
    "-r",
    "--locked",
    "--frozen",
    "--offline",
    "--no-fail-fast",
    "--quiet",
    "-q",
    "--verbose",
    "-v",
    "-vv",
    "--all-targets",
    "--tests",
    "--bins",
    "--examples",
    "--benches",
    "--timings",
    "--keep-going",
    "--ignore-rust-version",
    "--no-run",
];
const NEXTEST_VALUE_EXTRA: &[&str] = &[
    "-P",
    "-E",
    "--filterset",
    "--filter-expr",
    "--partition",
    "--test-threads",
    "--retries",
    "--cargo-profile",
    "--run-ignored",
    "--status-level",
    "--final-status-level",
    "--success-output",
    "--failure-output",
    "--no-tests",
    "--max-fail",
];
const NEXTEST_BOOLEAN_EXTRA: &[&str] = &["--no-capture", "--nocapture", "--fail-fast", "--hide-progress-bar"];
/// Target kinds: with one of these, crates are narrowed only as a whole.
const CARGO_TARGET_KINDS: &[&str] = &["--all-targets", "--tests", "--bins", "--examples", "--benches"];

fn cargo(before: &[String], args: &[String], cwd: &str) -> Option<Command> {
    let toolchain = usize::from(args.first().is_some_and(|a| a.starts_with('+'))); // cargo +nightly test
    let sub = args.get(toolchain).map(String::as_str);
    let call = concat(&[before, &args[..(toolchain + 1).min(args.len())]]);
    let rest: Vec<String> = args.get(toolchain + 1..).unwrap_or(&[]).to_vec();
    match sub {
        Some(s @ ("clippy" | "check" | "fmt")) => check(&format!("cargo {s}"), Ecosystem::Rust),
        Some("test") => cargo_tests(call, &rest, cwd, "cargo test", CARGO_VALUE.to_vec(), CARGO_BOOLEAN.to_vec()),
        Some("nextest") if rest.first().is_some_and(|r| r == "run") => cargo_tests(
            concat(&[&call, &strs(&["run"])]),
            &rest[1..],
            cwd,
            "cargo nextest",
            [CARGO_VALUE, NEXTEST_VALUE_EXTRA].concat(),
            [CARGO_BOOLEAN, NEXTEST_BOOLEAN_EXTRA].concat(),
        ),
        _ => None,
    }
}

fn cargo_tests(call: Vec<String>, args: &[String], cwd: &str, tool: &str, value: Vec<&str>, boolean: Vec<&str>) -> Option<Command> {
    let dash = args.iter().position(|a| a == "--");
    let harness: Vec<String> = dash.map_or(Vec::new(), |d| args[d..].to_vec());
    // A bare word is a test name filter, before `--` or after it.
    if !only_flags(harness.get(1..).unwrap_or(&[])) {
        return None;
    }
    let parsed = split_options(dash.map_or(args, |d| &args[..d]), &value, &boolean)?;
    if !parsed.targets.is_empty() {
        return None;
    }

    // Package selection is replaced by `-p` for each selected crate.
    let mut options: Vec<String> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();
    let mut i = 0;
    while i < parsed.options.len() {
        let o = &parsed.options[i];
        if o == "--exclude" {
            i += 1;
            excluded.push(parsed.options.get(i).cloned().unwrap_or_else(|| "undefined".into()));
        } else if let Some(v) = o.strip_prefix("--exclude=") {
            excluded.push(v.to_string());
        } else if o != "--workspace" && o != "--all" {
            options.push(o.clone());
        }
        i += 1;
    }
    let workspace_wide = parsed.options.iter().any(|o| o == "--workspace" || o == "--all");
    if !excluded.is_empty() && !workspace_wide {
        return None;
    }
    let scope = Rc::new(cargo_scope(cwd, workspace_wide, &excluded)?);
    let whole_crates = options.iter().any(|o| CARGO_TARGET_KINDS.contains(&o.as_str()));
    // nextest fails a run that selects no tests, as a crate without tests would.
    let no_tests: Vec<String> =
        if tool == "cargo nextest" && !options.iter().any(|o| o.starts_with("--no-tests")) { strs(&["--no-tests=pass"]) } else { vec![] };

    let cwd_owns = cwd.to_string();
    let cwd_narrow = cwd.to_string();
    Some(Command::Tests(TestCommand {
        tool: tool.into(),
        ecosystem: Ecosystem::Rust,
        why_not: None,
        owns: Some(Rc::new(move |file| scope.owns(&npath::resolve(&[&cwd_owns, file])))),
        narrow: Rc::new(move |files| {
            // Package name -> its selected integration targets, or None when the whole crate runs.
            let mut crates: IndexMap<String, Option<IndexSet<String>>> = IndexMap::new();
            for file in files {
                let target = cargo_target(&npath::resolve(&[&cwd_narrow, file]))?;
                match target.test {
                    Some(test) if !whole_crates => match crates.get_mut(&target.name) {
                        Some(None) => {}
                        Some(Some(set)) => {
                            set.insert(test);
                        }
                        None => {
                            crates.insert(target.name, Some(IndexSet::from([test])));
                        }
                    },
                    _ => {
                        crates.insert(target.name, None);
                    }
                }
            }
            // `--test` applies to every listed package, so one whole crate makes every crate whole.
            let partial = crates.values().all(Option::is_some);
            let mut tests: Vec<String> = if partial {
                crates.values().flatten().flatten().cloned().collect::<BTreeSet<String>>().into_iter().collect()
            } else {
                Vec::new()
            };
            crate::js::sort_strings(&mut tests);
            let mut names: Vec<String> = crates.keys().cloned().collect();
            crate::js::sort_strings(&mut names);
            let packages: Vec<String> = names.into_iter().flat_map(|n| ["-p".to_string(), n]).collect();
            let test_args: Vec<String> = tests.into_iter().flat_map(|t| ["--test".to_string(), t]).collect();
            Some(concat(&[&call, &options, &no_tests, &packages, &test_args, &harness]))
        }),
    }))
}

// Gradle options without a value. `-P`/`-D` properties and `--name=value` options are accepted too.
const GRADLE_BOOLEAN: &[&str] = &[
    "--no-daemon",
    "--daemon",
    "--stacktrace",
    "-s",
    "--full-stacktrace",
    "-S",
    "--info",
    "-i",
    "--debug",
    "-d",
    "--quiet",
    "-q",
    "--warn",
    "-w",
    "--scan",
    "--no-scan",
    "--parallel",
    "--no-parallel",
    "--build-cache",
    "--no-build-cache",
    "--configuration-cache",
    "--no-configuration-cache",
    "--offline",
    "--refresh-dependencies",
    "--rerun-tasks",
    "--continue",
    "--watch-fs",
    "--no-watch-fs",
    "--profile",
];
const GRADLE_VALUE: &[&str] = &["-x", "--exclude-task"];
/// Tasks that only compile, lint or verify (`check` runs `test` too): run in full or skipped.
const GRADLE_CHECKS: &[&str] = &[
    "check",
    "detekt",
    "detektMain",
    "detektTest",
    "ktlintCheck",
    "lintKotlin",
    "spotlessCheck",
    "spotlessKotlinCheck",
    "apiCheck",
    "lint",
    "compileKotlin",
    "compileTestKotlin",
    "compileJava",
    "compileTestJava",
];

/// Options that move the build somewhere else, so test files no longer map to its projects:
/// `/^--(?:project-dir|settings-file|build-file|include-build|tests)\b/`.
fn gradle_relocates(a: &str) -> bool {
    ["project-dir", "settings-file", "build-file", "include-build", "tests"].iter().any(|o| {
        a.strip_prefix("--")
            .and_then(|r| r.strip_prefix(o))
            .is_some_and(|rest| !rest.bytes().next().is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_'))
    })
}

fn gradle(before: &[String], args: &[String], cwd: &str) -> Option<Command> {
    let mut tasks: Vec<String> = Vec::new();
    let mut flags: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if !a.starts_with('-') {
            tasks.push(a.clone());
        } else if GRADLE_VALUE.contains(&a.as_str()) && i + 1 < args.len() {
            flags.push(a.clone());
            i += 1;
            flags.push(args[i].clone());
        } else if gradle_relocates(a) {
            return None;
        } else if GRADLE_BOOLEAN.contains(&a.as_str()) || is_property(a) || is_long_assignment(a) {
            flags.push(a.clone());
        } else {
            return None;
        }
        i += 1;
    }
    if !tasks.is_empty() && tasks.iter().all(|t| GRADLE_CHECKS.contains(&t.rsplit(':').next().unwrap_or(""))) {
        return check(&format!("gradle {}", tasks.join(" ")), Ecosystem::Kotlin);
    }
    if tasks.len() != 1 || tasks[0] != "test" {
        return None;
    }
    let build = Rc::new(gradle_build(cwd)?);
    let before = before.to_vec();
    let owns_build = Rc::clone(&build);
    let cwd_owns = cwd.to_string();
    let cwd_narrow = cwd.to_string();

    Some(Command::Tests(TestCommand {
        tool: "gradle test".into(),
        ecosystem: Ecosystem::Kotlin,
        owns: Some(Rc::new(move |file| owns_build.project_of(&npath::resolve(&[&cwd_owns, file])).is_some())),
        why_not: Some("Android and Kotlin Multiplatform test tasks take no --tests filter".into()),
        narrow: Rc::new(move |files| {
            // `test` runs every project's tests; each selected project's task gets its own `--tests` filters.
            let mut tasks: IndexMap<String, IndexSet<String>> = IndexMap::new();
            for file in files {
                let abs = npath::resolve(&[&cwd_narrow, file]);
                let project = build.project_of(&abs)?;
                let classes = jvm_test_classes(&abs);
                if classes.is_empty() || build.cant_filter(&project) {
                    return None;
                }
                let set = tasks.entry(project.path.clone()).or_default();
                // The trailing * keeps nested (@Nested) classes on every Gradle version.
                for c in classes {
                    set.insert(format!("{c}*"));
                }
            }
            let init = gradle_init_script()?;
            let mut entries: Vec<(String, IndexSet<String>)> = tasks.into_iter().collect();
            entries.sort_by(
                |(a, _), (b, _)| if crate::js::utf16_cmp(a, b).is_lt() { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater },
            );
            let mut selected = Vec::new();
            for (project, classes) in entries {
                selected.push(format!("{}:test", if project == ":" { "" } else { &project }));
                let mut classes: Vec<String> = classes.into_iter().collect();
                crate::js::sort_strings(&mut classes);
                for c in classes {
                    selected.push("--tests".into());
                    selected.push(c);
                }
            }
            Some(concat(&[&before, &flags, &["--init-script".to_string(), init], &selected]))
        }),
    }))
}

/// `/^-[PD]\S/`
fn is_property(a: &str) -> bool {
    let mut c = a.chars();
    c.next() == Some('-') && matches!(c.next(), Some('P' | 'D')) && c.next().is_some_and(|ch| !crate::js::is_space(ch))
}

/// `/^--[\w-]+=/`
fn is_long_assignment(a: &str) -> bool {
    a.strip_prefix("--")
        .and_then(|r| r.split_once('='))
        .is_some_and(|(name, _)| !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'))
}

const KTLINT_VALUE: &[&str] =
    &["--reporter", "--editorconfig", "--baseline", "--log-level", "-l", "--limit", "--color-name", "--ruleset", "-R", "--code-style"];
const KTLINT_BOOLEAN: &[&str] = &["--relative", "-F", "--format", "--color", "--verbose", "--debug", "--android", "-a", "--experimental"];

fn ktlint(before: Vec<String>, args: &[String]) -> Option<Command> {
    let parsed = split_options(args, KTLINT_VALUE, KTLINT_BOOLEAN)?;
    // Explicit patterns are globs (`src/**/*.kt`, `!**/generated/**`): checked in full or skipped.
    if !parsed.targets.is_empty() {
        return check("ktlint", Ecosystem::Kotlin);
    }
    let options = parsed.options;
    Some(Command::Files(FilesCommand {
        tool: "ktlint".into(),
        ecosystem: Ecosystem::Kotlin,
        targets: strs(&["."]),
        with_dependents: false,
        narrow: Rc::new(move |files| concat(&[&before, &options, files])),
    }))
}

// ESLint options that take a value as the next word. Any other option without `=` must be boolean.
const ESLINT_VALUE: &[&str] = &[
    "-c",
    "--config",
    "--ext",
    "--global",
    "--parser",
    "--parser-options",
    "--resolve-plugins-relative-to",
    "--plugin",
    "--rule",
    "--rulesdir",
    "--ignore-path",
    "--ignore-pattern",
    "-f",
    "--format",
    "-o",
    "--output-file",
    "--max-warnings",
    "--cache-location",
    "--cache-strategy",
    "--env",
    "--fix-type",
    "--report-unused-disable-directives-severity",
    "--flag",
    "--concurrency",
];
const ESLINT_BOOLEAN: &[&str] = &[
    "--fix",
    "--fix-dry-run",
    "--no-eslintrc",
    "--no-config-lookup",
    "--cache",
    "--quiet",
    "--color",
    "--no-color",
    "--no-inline-config",
    "--report-unused-disable-directives",
    "--no-error-on-unmatched-pattern",
    "--exit-on-fatal-error",
    "--no-warn-ignored",
    "--pass-on-no-patterns",
    "--debug",
    "--stats",
    "--no-ignore",
];

fn eslint(before: Vec<String>, args: &[String], cwd: &str) -> Option<Command> {
    let parsed = split_options(args, ESLINT_VALUE, ESLINT_BOOLEAN)?;
    let config = find_up(
        cwd,
        &["eslint.config.js", "eslint.config.mjs", "eslint.config.cjs", "eslint.config.ts", "eslint.config.mts", "eslint.config.cts"],
    );
    // Legacy .eslintrc configs lint explicitly passed files they would skip in a directory walk,
    // and import/no-unused-modules reports in a file's dependencies; both run whole or not at all.
    let whole = match &config {
        None => true,
        Some(c) => parsed.options.iter().any(|o| o == "-c" || o.starts_with("--config")) || read_text(c).contains("no-unused-modules"),
    };
    if whole {
        return check("eslint", Ecosystem::Js);
    }
    let options = if parsed.options.iter().any(|o| o == "--no-warn-ignored") {
        parsed.options
    } else {
        concat(&[&parsed.options, &strs(&["--no-warn-ignored"])])
    };
    Some(Command::Files(FilesCommand {
        tool: "eslint".into(),
        ecosystem: Ecosystem::Js,
        targets: if parsed.targets.is_empty() { strs(&["."]) } else { parsed.targets },
        with_dependents: true,
        narrow: Rc::new(move |files| concat(&[&before, &options, files])),
    }))
}

const PRETTIER_VALUE: &[&str] = &[
    "--config",
    "--ignore-path",
    "--log-level",
    "--plugin",
    "--parser",
    "--cache-location",
    "--cache-strategy",
    "--config-precedence",
    "--print-width",
    "--tab-width",
    "--trailing-comma",
    "--quote-props",
    "--arrow-parens",
    "--prose-wrap",
    "--end-of-line",
    "--html-whitespace-sensitivity",
    "--embedded-language-formatting",
    "--object-wrap",
    "--experimental-operator-position",
];
const PRETTIER_BOOLEAN: &[&str] = &[
    "--check",
    "-c",
    "--list-different",
    "-l",
    "--write",
    "-w",
    "--no-semi",
    "--single-quote",
    "--jsx-single-quote",
    "--bracket-same-line",
    "--no-bracket-spacing",
    "--use-tabs",
    "--cache",
    "--no-config",
    "--no-editorconfig",
    "--ignore-unknown",
    "-u",
    "--no-error-on-unmatched-pattern",
    "--with-node-modules",
    "--vue-indent-script-and-style",
    "--single-attribute-per-line",
    "--experimental-ternaries",
    "--color",
    "--no-color",
    "--require-pragma",
    "--insert-pragma",
];

fn prettier(before: Vec<String>, args: &[String]) -> Option<Command> {
    let parsed = split_options(args, PRETTIER_VALUE, PRETTIER_BOOLEAN)?;
    if parsed.targets.is_empty() {
        return None;
    }
    // --ignore-unknown: prettier errors on a file it can't parse when it's named explicitly.
    let unknown = parsed.options.iter().any(|o| o == "--ignore-unknown" || o == "-u");
    let options = if unknown { parsed.options } else { concat(&[&parsed.options, &strs(&["--ignore-unknown"])]) };
    Some(Command::Files(FilesCommand {
        tool: "prettier".into(),
        ecosystem: Ecosystem::Js,
        targets: parsed.targets,
        with_dependents: false,
        narrow: Rc::new(move |files| concat(&[&before, &options, files])),
    }))
}

struct Parsed {
    options: Vec<String>,
    targets: Vec<String>,
}

/// Separates options from target paths. None when an option is unknown, so a value can't pass for a path.
fn split_options(args: &[String], value: &[&str], boolean: &[&str]) -> Option<Parsed> {
    let mut options = Vec::new();
    let mut targets = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if !a.starts_with('-') {
            targets.push(a.clone());
        } else if a.find('=').is_some_and(|eq| value.contains(&&a[..eq])) {
            options.push(a.clone());
        } else if value.contains(&a.as_str()) && i + 1 < args.len() {
            options.push(a.clone());
            i += 1;
            options.push(args[i].clone());
        } else if boolean.contains(&a.as_str()) {
            options.push(a.clone());
        } else {
            return None;
        }
        i += 1;
    }
    Some(Parsed { options, targets })
}

/// Major version of phpunit/phpunit in the nearest composer.lock, or 0 when unknown.
fn phpunit_major(cwd: &str) -> Result<f64, String> {
    let lock = find_up(cwd, &["composer.lock"]);
    let json = lock.as_deref().and_then(json::read);
    let mut packages: Vec<Value> = Vec::new();
    for key in ["packages", "packages-dev"] {
        match json.as_ref().and_then(|j| json::get(j, key)) {
            None | Some(Value::Null) => {}
            Some(Value::Array(items)) => packages.extend(items.iter().cloned()),
            // A string spreads into its characters, none of which has a name.
            Some(Value::String(_)) => {}
            Some(_) => return Err("((intermediate value) ?? []) is not iterable".into()),
        }
    }
    let pkg = packages.iter().find(|p| json::get(p, "name").and_then(Value::as_str) == Some("phpunit/phpunit"));
    let version = pkg.and_then(|p| json::get(p, "version")).filter(|v| !v.is_null()).map_or(String::new(), json::to_js_string);
    // /^v?(\d+)/
    let digits: String = version.strip_prefix('v').unwrap_or(&version).chars().take_while(char::is_ascii_digit).collect();
    Ok(if digits.is_empty() { 0.0 } else { crate::js::parse_number(&digits) })
}

fn local_bin(cwd: &str, tool: &str) -> Option<String> {
    let mut dir = cwd.to_string();
    loop {
        let bin = npath::join(&[&dir, "node_modules/.bin", tool]);
        if exists(&bin) {
            return Some(bin);
        }
        let parent = npath::dirname(&dir);
        if exists(&npath::join(&[&dir, ".git"])) || parent == dir {
            return None;
        }
        dir = parent;
    }
}

/// The first of `names` found in `cwd` or a parent, up to the repository root.
pub fn find_up(cwd: &str, names: &[&str]) -> Option<String> {
    let mut dir = cwd.to_string();
    loop {
        for name in names {
            let p = npath::join(&[&dir, name]);
            if exists(&p) {
                return Some(p);
            }
        }
        let parent = npath::dirname(&dir);
        if exists(&npath::join(&[&dir, ".git"])) || parent == dir {
            return None;
        }
        dir = parent;
    }
}

pub fn read_text(file: &str) -> String {
    std::fs::read(file).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

// A bare word after the command may be a test filter or a flag's value; both make
// narrowing unsafe, so only `--flag` / `--flag=value` arguments are accepted.
fn only_flags(args: &[String]) -> bool {
    args.iter().all(|a| a.starts_with('-'))
}
